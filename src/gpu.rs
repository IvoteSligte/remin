use std::sync::Arc;

use gpu_video::{
    BytesDecoder as BytesDecoderH264, BytesEncoderH264, DecoderError, EncodedInputChunk, OutputFrame, RawFrameData, VulkanDevice, parameters::{DecoderParameters, EncoderParametersH264, RateControl, VideoParameters}
};

use crate::server::FRAME_RATE;

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

pub struct Decoder {
    h264_decoder: BytesDecoderH264,
}

impl Decoder {
    pub fn new(device: Arc<VulkanDevice>) -> Result<Self, DecoderError> {
        let h264_decoder = device.create_bytes_decoder_h264(DecoderParameters::default())?;
        Ok(Self { h264_decoder })
    }

    pub fn decode(&mut self, data: &[u8]) -> Result<Option<OutputFrame<RawFrameData>>, DecoderError> {
        let yuv_frames = self.h264_decoder.decode(EncodedInputChunk {
            data,
            pts: None, // TODO: synchronisation timestamp
        })?;
        let maybe_yuv_frame = yuv_frames.into_iter().next();
        Ok(maybe_yuv_frame)
    }
}
