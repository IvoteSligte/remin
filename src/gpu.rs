use std::{iter, sync::Arc, time::Instant};

use gpu_video::{
    BytesEncoderH264, EncodedInputChunk, VulkanDevice, VulkanEncoderError, WgpuNv12ToRgbaConverter,
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
use yuv::{
    BufferStoreMut, YuvBiPlanarImageMut, YuvChromaSubsampling, YuvConversionMode, YuvRange,
    YuvStandardMatrix,
};

use crate::{App, server::FRAME_RATE};

fn bgra_to_yuv(bgra: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut image = YuvBiPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
    yuv::bgra_to_yuv_nv12(
        &mut image,
        bgra,
        stride,
        YuvRange::Limited,
        YuvStandardMatrix::Bt709,
        YuvConversionMode::Balanced,
    )
    .unwrap();
    let BufferStoreMut::Owned(y_plane) = image.y_plane else {
        unreachable!();
    };
    let BufferStoreMut::Owned(uv_plane) = image.uv_plane else {
        unreachable!();
    };
    Vec::from_iter(iter::chain(y_plane, uv_plane))
}

pub struct Encoder {
    nv12_to_h264: BytesEncoderH264,
    width: u32,
    height: u32,
    stride: u32,
}

impl Encoder {
    pub fn new(
        device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
        stride: u32,
        format: janck::Format,
    ) -> Self {
        // TODO: support other formats
        assert_eq!(format, janck::Format::Bgra8);

        let nv12_to_h264 = device
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
            .unwrap();
        Self {
            nv12_to_h264,
            width,
            height,
            stride,
        }
    }

    // Encode BGRA frame to H.264
    pub fn encode(&mut self, bgra_bytes: &[u8]) -> Result<Vec<u8>, VulkanEncoderError> {
        let yuv_frame = bgra_to_yuv(&bgra_bytes, self.width, self.height, self.stride);
        let encoded = self.nv12_to_h264.encode(
            &gpu_video::InputFrame {
                data: gpu_video::RawFrameData {
                    frame: yuv_frame,
                    width: self.width,
                    height: self.height,
                },
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
    ConverterInit(#[from] gpu_video::WgpuConverterInitError),

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
        let rgba_texture = wgpu_device.create_texture(&TextureDescriptor {
            label: None,
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
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
