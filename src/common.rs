use std::{collections::HashSet, time::Instant};

use chrono::{DateTime, Utc};
use wincode::{SchemaRead, SchemaWrite};

pub const HOST_PORT: u16 = 8084;

#[derive(Debug, Default, Clone, SchemaWrite, SchemaRead)]
pub struct Input {
    pub keys_pressed: HashSet<char>,
    pub mouse_position: [f64; 2],
    pub left_mouse_pressed: bool,
    pub middle_mouse_pressed: bool,
    pub right_mouse_pressed: bool,
    pub scroll: [f64; 2], // FIXME
}

#[derive(SchemaWrite, SchemaRead)]
pub enum Packet<'a> {
    /// Input state
    Input(Input),
    /// H.264 video fragment
    H264 {
        width: u32,
        height: u32,
        bytes: &'a [u8],
        /// Microseconds since UNIX epoch
        timestamp: i64,
    },
}

/// Returns the time in milliseconds since `start`
pub(crate) fn since(start: Instant) -> f32 {
    (Instant::now() - start).as_micros() as f32 / 1000.0
}

#[derive(Clone, Copy)]
pub(crate) struct TimeStamp(DateTime<Utc>);

impl TimeStamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }

    /// Returns the time in milliseconds since `self`
    pub fn since(&self) -> f32 {
        (Utc::now() - self.0).num_microseconds().unwrap_or(i64::MAX) as f32 / 1000.0
    }

    pub fn raw(&self) -> i64 {
        self.0.timestamp_micros()
    }

    pub fn from_raw(micros: i64) -> Self {
        Self(DateTime::from_timestamp_micros(micros).unwrap_or(DateTime::<Utc>::MAX_UTC))
    }
}
