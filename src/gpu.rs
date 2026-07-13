use std::{sync::Arc, time::Instant};

use gpu_video::{
    EncodedInputChunk, VulkanDevice, VulkanEncoderError,
    WgpuTexturesDecoder as WgpuTexturesDecoderH264, WgpuTexturesEncoderH264,
    parameters::{
        ColorRange, ColorSpace, DecoderParameters, EncoderParametersH264, RateControl,
        VideoParameters,
    },
};
use log::{info, trace, warn};
use netnet::Signal;
use slint::{ComponentHandle, Weak};
use thiserror::Error;
use wgpu::{
    BindGroup, CommandEncoderDescriptor, Device, Extent3d, Origin3d, Queue, TexelCopyBufferLayout,
    TexelCopyTextureInfo, Texture, TextureAspect, TextureDescriptor, TextureDimension,
    TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};

mod wgpu_helpers;
use wgpu_helpers::{WgpuConverterParameters, WgpuNv12ToRgbaConverter, WgpuRgbaToNv12Converter};

use crate::{App, server::FRAME_RATE};

fn create_texture(
    device: &Device,
    width: u32,
    height: u32,
    format: TextureFormat,
    usage: TextureUsages,
) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

#[derive(Error, Debug)]
pub enum EncoderError {
    #[error(transparent)]
    Encode(#[from] gpu_video::VulkanEncoderError),

    #[error(transparent)]
    ConverterInit(#[from] wgpu_helpers::WgpuConverterInitError),
}

pub struct Encoder {
    device: Device,
    queue: Queue,
    /// Must have a format that can be trivially converted to RGBA (e.g. BGRA, RGBA)
    input_texture: Texture,
    input_texture_bind_group: BindGroup,
    nv12_y_plane_view: TextureView,
    nv12_uv_plane_view: TextureView,
    rgba_to_nv12: WgpuRgbaToNv12Converter,
    nv12_to_h264: WgpuTexturesEncoderH264,
    stride: u32,
}

impl Encoder {
    pub fn new(
        device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
        stride: u32,
        format: janck::Format,
    ) -> Result<Self, EncoderError> {
        assert!(stride >= width * 4);
        let format = match format {
            // using BGRA8 as texture format makes WGPU automatically map
            // color.r, color.g color.b to the correct values,
            // which is all that wgpu_helpers/rgba_to_nv12.wgsl needs
            janck::Format::Bgra8 => TextureFormat::Bgra8Unorm,
            // RGBA8 can be used as-is
            janck::Format::Rgba8 => TextureFormat::Rgba8Unorm,
            _ => unimplemented!(),
        };
        let wgpu_device = device.wgpu_device();
        let input_texture = create_texture(
            &wgpu_device,
            width,
            height,
            format,
            TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        );
        let nv12_texture = create_texture(
            &wgpu_device,
            width,
            height,
            TextureFormat::NV12,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        );
        let nv12_y_plane_view = nv12_texture.create_view(&TextureViewDescriptor {
            aspect: TextureAspect::Plane0,
            ..Default::default()
        });
        let nv12_uv_plane_view = nv12_texture.create_view(&TextureViewDescriptor {
            aspect: TextureAspect::Plane1,
            ..Default::default()
        });
        let rgba_to_nv12 = WgpuRgbaToNv12Converter::new(
            &wgpu_device,
            WgpuConverterParameters {
                color_space: ColorSpace::BT709,
                color_range: ColorRange::Limited,
            },
            format,
        )?;
        let nv12_to_h264 = device.create_wgpu_textures_encoder_h264(EncoderParametersH264 {
            input_parameters: VideoParameters {
                width: width.try_into().unwrap(),
                height: height.try_into().unwrap(),
                target_framerate: (FRAME_RATE as u32).into(),
            },
            output_parameters: device
                .encoder_output_parameters_h264_low_latency(RateControl::Disabled)
                .unwrap(),
        })?;
        Ok(Self {
            device: wgpu_device,
            queue: device.wgpu_queue(),
            input_texture_bind_group: rgba_to_nv12.create_input_bind_group(&input_texture),
            input_texture,
            nv12_y_plane_view,
            nv12_uv_plane_view,
            rgba_to_nv12,
            nv12_to_h264,
            stride,
        })
    }

    // Encode BGRA frame to H.264
    pub fn encode(&mut self, bytes: &[u8]) -> Result<Vec<u8>, VulkanEncoderError> {
        self.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &self.input_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            bytes,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.stride),
                rows_per_image: None,
            },
            self.input_texture.size(),
        );
        let mut command_encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());

        self.rgba_to_nv12.convert(
            &mut command_encoder,
            &self.input_texture_bind_group,
            &self.nv12_y_plane_view,
            &self.nv12_uv_plane_view,
        );
        let command_buffer = command_encoder.finish();
        self.queue.submit(Some(command_buffer));
        let encoded = self.nv12_to_h264.encode(
            gpu_video::InputFrame {
                data: self.nv12_y_plane_view.texture().clone(),
                pts: None, // TODO: synchronisation timestamp (once there is audio)
            },
            false,
        )?;
        Ok(encoded.data)
    }
}

#[derive(Error, Debug)]
pub enum DecoderError {
    #[error(transparent)]
    H264(#[from] gpu_video::DecoderError),

    #[error(transparent)]
    ConverterInit(#[from] wgpu_helpers::WgpuConverterInitError),

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
