use std::{
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use log::{debug, info, warn};
use winit::{
    event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::PhysicalKey,
    window::{CursorGrabMode, Fullscreen, Window},
};
use wrapp::egui;

use crate::{
    common::Input,
    key::{Key, should_ignore_press},
};

enum UserEvent {
    ScheduledOnInput,
}

struct App {
    device: wgpu::Device,
    out_window: Arc<OnceLock<Weak<Window>>>,
    video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
    video_texture_id: Option<egui::TextureId>,
    is_focused: bool,
    input: Input,
    on_input: Box<dyn FnMut(&Input)>,
    /// True if an on_input event has been scheduled using [UserEvent::ScheduledOnInput].
    /// This is useful for aggregating input events that are otherwise emitted too often,
    /// such as raw mouse motion device events.
    on_input_scheduled: bool,
}

impl wrapp::Application for App {
    type UserEvent = UserEvent;

    fn resumed(&mut self, helper: wrapp::Helper<Self>) {
        self.out_window.set(Arc::downgrade(&helper.window)).unwrap();
    }

    fn render_ui(&mut self, helper: wrapp::Helper<Self>, ui: &mut egui::Ui) {
        let Some((video_texture_id, video_texture_width, video_texture_height)) =
            self.get_video_texture(helper)
        else {
            warn!("Trying to render, but the video texture view is not set yet");
            return;
        };
        let video_texture_image = {
            // NOTE: is the texture scaled to fit the screen? (when source resolution < screen resolution)
            egui::ImageSource::Texture(egui::load::SizedTexture {
                id: video_texture_id,
                size: egui::Vec2::new(video_texture_width as _, video_texture_height as _),
            })
        };
        ui.centered_and_justified(|ui| {
            ui.add(
                egui::Image::new(video_texture_image)
                    .maintain_aspect_ratio(true)
                    .fit_to_exact_size(ui.content_rect().size())
                    .max_size(ui.content_rect().size()),
            )
        });
    }

    fn window_event(&mut self, helper: wrapp::Helper<Self>, event: WindowEvent) {
        match event {
            WindowEvent::Focused(focused) => match focused {
                true => self.on_focus(helper),
                false => self.on_unfocus(helper),
            },
            WindowEvent::MouseWheel { delta, .. } => {
                match delta {
                    MouseScrollDelta::LineDelta(lines_x, lines_y) => {
                        self.input.scroll.0 += lines_x as f64;
                        self.input.scroll.1 += lines_y as f64;
                    }
                    MouseScrollDelta::PixelDelta(physical_position) => {
                        self.input.scroll.0 += physical_position.x;
                        self.input.scroll.1 += physical_position.y;
                    }
                }
                self.on_input()
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(key_code) = event.physical_key else {
                    warn!("Unidentified key: '{:?}'", event.physical_key);
                    return;
                };
                let Ok(key) = Key::try_from(key_code) else {
                    warn!("Unknown key: '{:?}'", key_code);
                    return;
                };
                if key == Key::F11 {
                    let window = helper.window;
                    match window.fullscreen() {
                        Some(_) => window.set_fullscreen(None),
                        None => window.set_fullscreen(Some(Fullscreen::Borderless(None))),
                    }
                    return;
                }
                match event.state {
                    ElementState::Pressed => {
                        if self.input.keys_pressed.contains(&key)
                            || should_ignore_press(&self.input.keys_pressed, key)
                        {
                            return;
                        }
                        self.input.keys_pressed.insert(key);
                        debug!("Pressed key: {:?}", key);
                    }
                    ElementState::Released => {
                        self.input.keys_pressed.remove(&key);
                        debug!("Released key: {:?}", key);
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
            WindowEvent::Occluded(_occluded) => warn!("Window occlusion is currently not handled"),
            _ => {}
        }
    }

    fn device_event(&mut self, helper: wrapp::Helper<Self>, event: DeviceEvent) {
        if !self.is_focused {
            return;
        }
        match event {
            DeviceEvent::MouseMotion { delta } => {
                let size = helper.window.inner_size();
                self.input.mouse_position.0 += delta.0 / size.width as f64;
                self.input.mouse_position.1 += delta.1 / size.height as f64;
                if !self.on_input_scheduled {
                    let proxy = helper.event_loop_proxy.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        let _ = proxy.send_event(UserEvent::ScheduledOnInput);
                    });
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _helper: wrapp::Helper<Self>, event: UserEvent) {
        match event {
            UserEvent::ScheduledOnInput => self.on_input(),
        }
    }

    fn before_drop(&mut self, egui_renderer: &mut wrapp::egui_wgpu::Renderer) {
        if let Some(video_texture_id) = self.video_texture_id.take() {
            egui_renderer.free_texture(&video_texture_id);
        }
    }
}

impl App {
    pub fn new(
        device: wgpu::Device,
        out_window: Arc<OnceLock<Weak<Window>>>,
        video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
        on_input: impl FnMut(&Input) + 'static,
    ) -> Self {
        Self {
            device,
            out_window,
            video_texture_view,
            video_texture_id: None,
            is_focused: false,
            input: Input::default(),
            on_input: Box::new(on_input),
            on_input_scheduled: false,
        }
    }

    fn get_video_texture(
        &mut self,
        helper: wrapp::Helper<Self>,
    ) -> Option<(egui::TextureId, u32, u32)> {
        let texture_view = self.video_texture_view.get()?;
        let texture_id = *self.video_texture_id.get_or_insert_with(|| {
            helper.egui_renderer.register_native_texture(
                &self.device,
                texture_view,
                wgpu::FilterMode::Linear,
            )
        });
        let texture = texture_view.texture();
        Some((texture_id, texture.width(), texture.height()))
    }

    // Callbacks are only called if the window exists, with the exception of on_exit

    fn on_input(&mut self) {
        debug!("Calling input callback");
        let Self { on_input, .. } = self;
        on_input(&self.input);
    }

    fn on_focus(&mut self, helper: wrapp::Helper<Self>) {
        self.is_focused = true;
        let window = helper.window.clone();
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

    fn on_unfocus(&mut self, helper: wrapp::Helper<Self>) {
        self.is_focused = false;
        let window = helper.window;
        window.set_cursor_grab(CursorGrabMode::None).unwrap();
        window.set_cursor_visible(true);
    }
}

/// This function *must* be called from the main thread
pub fn run_event_loop(
    instance: Arc<avec::Instance>,
    device: Arc<avec::Device>,
    out_window: Arc<OnceLock<Weak<Window>>>,
    video_texture_view: Arc<OnceLock<wgpu::TextureView>>,
    on_input: impl FnMut(&Input) + Send + 'static,
) {
    let app = App::new(
        device.wgpu_device(),
        out_window,
        video_texture_view,
        on_input,
    );
    wrapp::run(
        app,
        instance.wgpu_instance(),
        device.wgpu_queue(),
        device.wgpu_device(),
    )
    .unwrap();
    warn!("Event loop finished");
}
