use std::{sync::Arc, time::Instant};

use gpu_video::{
    VulkanDevice, VulkanEncoderError, WgpuTexturesEncoderH264,
    parameters::{ColorRange, ColorSpace, EncoderParametersH264, RateControl, VideoParameters}, wgpu_helpers::{WgpuConverterParameters, WgpuRgbaToNv12Converter},
};
use log::trace;
use thiserror::Error;
use wgpu::{
    BindGroup, CommandEncoderDescriptor, Device, Origin3d, Queue, TexelCopyBufferLayout,
    TexelCopyTextureInfo, Texture, TextureAspect, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};

use crate::common::since;

use super::create_texture;

#[derive(Error, Debug)]
pub enum EncoderError {
    #[error(transparent)]
    Encode(#[from] gpu_video::VulkanEncoderError),

    #[error(transparent)]
    ConverterInit(#[from] gpu_video::wgpu_helpers::WgpuConverterInitError),
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
        target_framerate: u32,
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
        )?;
        let nv12_to_h264 = device.create_wgpu_textures_encoder_h264(EncoderParametersH264 {
            input_parameters: VideoParameters {
                width: width.try_into().unwrap(),
                height: height.try_into().unwrap(),
                target_framerate: target_framerate.into(),
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
        let encoder_start = Instant::now();
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
        command_buffer.on_submitted_work_done(move || {
            trace!("RBGA-to-NV12 encoding took {:.2}ms", since(encoder_start));
        });
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
