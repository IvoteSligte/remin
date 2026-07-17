use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Instant};

use gpu_video::{
    EncodedInputChunk, VulkanDevice, WgpuTexturesDecoder as WgpuTexturesDecoderH264,
    parameters::{ColorRange, ColorSpace, DecoderParameters},
};
use log::{info, trace, warn};
use slint::{ComponentHandle, Weak};
use thiserror::Error;
use wgpu::{Device, Queue, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor};

use super::create_texture;
use super::wgpu_helpers::{WgpuConverterParameters, WgpuNv12ToRgbaConverter};

use crate::App;

#[derive(Default, Clone)]
pub struct Signal {
    value: Arc<AtomicBool>,
}

impl Signal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        self.value.store(true, Ordering::Release);
    }

    pub fn clear(&self) {
        self.value.store(false, Ordering::Release);
    }

    pub fn get(&self) -> bool {
        self.value.load(Ordering::Acquire)
    }
}

#[derive(Error, Debug)]
pub enum DecoderError {
    #[error(transparent)]
    H264(#[from] gpu_video::DecoderError),

    #[error(transparent)]
    ConverterInit(#[from] super::wgpu_helpers::WgpuConverterInitError),

    #[error("The provided data was not enough to produce a new frame")]
    NoNewFrame,
}

pub struct Decoder {
    h264_to_nv12: WgpuTexturesDecoderH264,
    nv12_to_rgba: WgpuNv12ToRgbaConverter,
    rgba_texture_view: (TextureView, Signal),
    device: Device,
    queue: Queue,
}

impl Decoder {
    // NOTE: this assumes that Slint only uses one queue internally
    pub fn new(
        device: Arc<VulkanDevice>,
        queue: Queue,
        weak_app: Weak<App>,
        width: u32,
        height: u32,
    ) -> Result<Self, DecoderError> {
        info!("Creating H264-to-RGBA decoder");
        let h264_to_nv12 = device.create_wgpu_textures_decoder_h264(DecoderParameters::default())?;
        let wgpu_device = device.wgpu_device();
        let nv12_to_rgba = WgpuNv12ToRgbaConverter::new(
            &wgpu_device,
            WgpuConverterParameters {
                color_space: ColorSpace::BT709,
                color_range: ColorRange::Limited,
            },
        )?;
        info!("Creating RGBA video frame texture");
        // TODO: double-buffering?
        let rgba_texture = create_texture(
            &wgpu_device,
            width,
            height,
            TextureFormat::Rgba8Unorm,
            TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT,
        );
        let rgba_texture2 = rgba_texture.clone();
        let in_use_signal = Signal::new();
        let in_use_signal2 = in_use_signal.clone();
        let weak_app2 = weak_app.clone();
        weak_app
            .upgrade_in_event_loop(move |app| {
                app.set_video_frame(slint::Image::try_from(rgba_texture2).unwrap());
                app.window()
                    .set_rendering_notifier(move |state, _| match state {
                        slint::RenderingState::BeforeRendering => {
                            if let Some(app) = weak_app2.upgrade() {
                                trace!("Redrawing window");
                                in_use_signal2.set();
                                // It is necessary to request a redraw because Slint is
                                // not aware of us changing the video frame image
                                app.window().request_redraw();
                            }
                        }
                        slint::RenderingState::AfterRendering => {
                            in_use_signal2.clear();
                        }
                        _ => (),
                    })
                    .unwrap();
            })
            .unwrap();
        let view = rgba_texture.create_view(&TextureViewDescriptor::default());
        Ok(Self {
            h264_to_nv12,
            nv12_to_rgba,
            device: device.wgpu_device(),
            rgba_texture_view: (view, in_use_signal),
            queue,
        })
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<(), DecoderError> {
        trace!("Decoding H264 data");
        let start_instant = Instant::now();
        let nv12_frames = self.h264_to_nv12.decode(EncodedInputChunk {
            data,
            pts: None, // TODO: synchronisation timestamp
        })?;
        trace!(
            "H264-to-NV12 decoding took {:.2}ms",
            (Instant::now() - start_instant).as_micros() as f32 / 1000.0
        );
        // As the encoder splits each frame into one or more packets,
        // one packet should never correspond to more than one frame
        debug_assert!(nv12_frames.len() <= 1);

        let Some(nv12_frame) = nv12_frames.into_iter().next() else {
            return Err(DecoderError::NoNewFrame);
        };
        let command_encoder_start = Instant::now();
        let mut command_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bind_group = self.nv12_to_rgba.create_input_bind_group(&nv12_frame)?;
        let (rgba_texture_view, in_use) = &self.rgba_texture_view;
        if in_use.get() {
            warn!("Texture already in use. Skipping decoding.");
            return Ok(());
        }
        self.nv12_to_rgba
            .convert(&mut command_encoder, &bind_group, rgba_texture_view);
        let command_buffer = command_encoder.finish();
        trace!(
            "Creating the NV12-to-RGBA command buffer took {:.2}ms",
            (Instant::now() - command_encoder_start).as_micros() as f32 / 1000.0
        );
        let submit_start = Instant::now();
        self.queue.submit(Some(command_buffer));
        trace!(
            "Submitting the command buffer took {:.2}ms",
            (Instant::now() - submit_start).as_micros() as f32 / 1000.0
        );
        Ok(())
    }
}
