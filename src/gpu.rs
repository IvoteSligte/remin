use std::{sync::Arc, time::Instant};

use gpu_video::{
    BytesEncoderH264, EncodedInputChunk, VulkanDevice, WgpuNv12ToRgbaConverter,
    WgpuTexturesDecoder as WgpuTexturesDecoderH264,
    parameters::{
        ColorRange, ColorSpace, DecoderParameters, EncoderParametersH264, RateControl,
        VideoParameters, WgpuConverterParameters,
    },
};
use log::{info, trace, warn};
use netnet::Signal;
use slint::{ComponentHandle, Weak};
use thiserror::Error;
use wgpu::{
    Device, Extent3d, Queue, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    TextureView, TextureViewDescriptor,
};

use crate::{App, server::FRAME_RATE};

pub fn create_encoder(device: &Arc<VulkanDevice>, width: u32, height: u32) -> BytesEncoderH264 {
    device
        .create_bytes_encoder_h264(EncoderParametersH264 {
            input_parameters: VideoParameters {
                width: width.try_into().unwrap(),
                height: height.try_into().unwrap(),
                target_framerate: (FRAME_RATE as u32).into(),
            },
            output_parameters: device
                .encoder_output_parameters_h264_low_latency(RateControl::Disabled)
                .unwrap(),
        })
        .unwrap()
}

#[derive(Error, Debug)]
pub enum DecoderError {
    #[error(transparent)]
    H264(#[from] gpu_video::DecoderError),

    #[error(transparent)]
    ConverterInit(#[from] gpu_video::WgpuConverterInitError),

    #[error("The provided data was not enough to produce a new frame")]
    NoNewFrame,
}

pub struct Decoder {
    h264_to_nv12: WgpuTexturesDecoderH264,
    nv12_to_rgba: WgpuNv12ToRgbaConverter,
    rgba_texture_view: Option<(TextureView, Signal)>,
    device: Device,
    queue: Queue,
    weak_app: Weak<App>,
}

impl Decoder {
    // NOTE: this assumes that Slint only uses one queue internally
    pub fn new(
        device: Arc<VulkanDevice>,
        queue: Queue,
        weak_app: Weak<App>,
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
        Ok(Self {
            h264_to_nv12,
            nv12_to_rgba,
            device: device.wgpu_device(),
            rgba_texture_view: None,
            queue,
            weak_app,
        })
    }

    fn finish_init(&mut self, frame_size: Extent3d) {
        info!("Creating RGBA video frame texture");
        let rgba_texture = self.device.create_texture(&TextureDescriptor {
            label: None,
            size: frame_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let rgba_texture2 = rgba_texture.clone();
        let weak_app = self.weak_app.clone();
        let in_use_signal = Signal::new();
        let in_use_signal2 = in_use_signal.clone();
        self.weak_app
            .upgrade_in_event_loop(move |app| {
                app.set_video_frame(slint::Image::try_from(rgba_texture2).unwrap());
                app.window()
                    .set_rendering_notifier(move |state, _| match state {
                        slint::RenderingState::BeforeRendering => {
                            if let Some(app) = weak_app.upgrade() {
                                trace!("Redrawing window");
                                in_use_signal2.set();
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
        self.rgba_texture_view = Some((view, in_use_signal));
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
        if self.rgba_texture_view.is_none() {
            self.finish_init(nv12_frame.data.size());
        }
        let command_encoder_start = Instant::now();
        let mut command_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bind_group = self.nv12_to_rgba.create_input_bind_group(&nv12_frame)?;
        let (rgba_texture_view, in_use) = self.rgba_texture_view.as_ref().unwrap();
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
