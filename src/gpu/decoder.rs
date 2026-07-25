use std::{sync::Arc, time::Instant};

use gpu_video::{
    EncodedInputChunk, VulkanDevice, WgpuNv12ToRgbaConverter,
    WgpuTexturesDecoder as WgpuTexturesDecoderH264,
    parameters::{ColorRange, ColorSpace, DecoderParameters, WgpuConverterParameters},
};
use log::{info, trace};
use thiserror::Error;
use wgpu::{Device, Queue, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor};

use super::create_texture;

use crate::common::since;

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
    rgba_texture_view: TextureView,
    device: Device,
    queue: Queue,
}

impl Decoder {
    // NOTE: this assumes that Slint only uses one queue internally
    pub fn new(
        device: Arc<VulkanDevice>,
        queue: Queue,
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
        let rgba_texture_view = rgba_texture.create_view(&TextureViewDescriptor::default());
        Ok(Self {
            h264_to_nv12,
            nv12_to_rgba,
            device: device.wgpu_device(),
            rgba_texture_view,
            queue,
        })
    }

    pub fn output_texture_view(&self) -> &TextureView {
        &self.rgba_texture_view
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<(), DecoderError> {
        trace!("Decoding H264 data");
        let decode_start = Instant::now();
        let nv12_frames = self.h264_to_nv12.decode(EncodedInputChunk {
            data,
            pts: None, // TODO: synchronisation timestamp
        })?;
        trace!("H264-to-NV12 decoding took {:.2}ms", since(decode_start));
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
        self.nv12_to_rgba
            .convert(&mut command_encoder, &bind_group, &self.rgba_texture_view);
        let command_buffer = command_encoder.finish();
        trace!(
            "Creating the NV12-to-RGBA command buffer took {:.2}ms",
            since(command_encoder_start)
        );
        command_buffer.on_submitted_work_done(move || {
            trace!(
                "NV12-to-RGBA decoding pipeline took {:.2}ms",
                since(command_encoder_start)
            );
        });
        let submit_start = Instant::now();
        self.queue.submit(Some(command_buffer));
        trace!(
            "Submitting the command buffer took {:.2}ms",
            (Instant::now() - submit_start).as_micros() as f32 / 1000.0
        );
        Ok(())
    }
}
