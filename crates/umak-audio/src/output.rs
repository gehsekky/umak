//! The output stage. The engine only knows this trait, so tests can capture samples and
//! a later WASAPI exclusive backend can sit beside the cpal one.

use std::any::Any;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::render::{Renderer, Shared};

/// Samples converted per pass for devices that do not take floats.
const CONVERT_BLOCK: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFormat {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
}

pub trait AudioOutput {
    fn format(&self) -> OutputFormat;
    /// Starts pulling samples. Output continues until the returned handle is dropped.
    fn start(self: Box<Self>, renderer: Renderer) -> Result<Box<dyn Any>, String>;
}

/// The system's default output device.
pub struct CpalOutput {
    device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    format: OutputFormat,
}

impl CpalOutput {
    pub fn default_device() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device was found")?;
        let supported = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "default output".to_owned());
        let format = OutputFormat {
            device: name,
            sample_rate: supported.sample_rate(),
            channels: supported.channels(),
        };
        Ok(Self {
            device,
            sample_format: supported.sample_format(),
            config: supported.config(),
            format,
        })
    }

    fn stream<T>(self, mut renderer: Renderer) -> Result<cpal::Stream, String>
    where
        T: SizedSample + FromSample<f32> + Send + 'static,
    {
        let shared: Arc<Shared> = Arc::clone(renderer.shared());
        let mut floats = vec![0.0_f32; CONVERT_BLOCK * renderer.channels()];
        let block = floats.len();
        self.device
            .build_output_stream::<T, _, _>(
                self.config,
                move |data: &mut [T], _| {
                    for chunk in data.chunks_mut(block) {
                        let floats = &mut floats[..chunk.len()];
                        renderer.render(floats);
                        for (out, sample) in chunk.iter_mut().zip(floats.iter()) {
                            *out = T::from_sample(*sample);
                        }
                    }
                },
                move |error| {
                    eprintln!("audio output error: {error}");
                    shared.mark_output_failed();
                },
                None,
            )
            .map_err(|error| error.to_string())
    }
}

impl AudioOutput for CpalOutput {
    fn format(&self) -> OutputFormat {
        self.format.clone()
    }

    fn start(self: Box<Self>, renderer: Renderer) -> Result<Box<dyn Any>, String> {
        let stream = match self.sample_format {
            SampleFormat::F32 => self.stream::<f32>(renderer),
            SampleFormat::F64 => self.stream::<f64>(renderer),
            SampleFormat::I32 => self.stream::<i32>(renderer),
            SampleFormat::I16 => self.stream::<i16>(renderer),
            SampleFormat::U16 => self.stream::<u16>(renderer),
            SampleFormat::U8 => self.stream::<u8>(renderer),
            other => Err(format!("unsupported output sample format: {other:?}")),
        }?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Box::new(stream))
    }
}

/// Discards audio at the speed of a real device, for running with no sound.
pub struct NullOutput {
    format: OutputFormat,
}

impl NullOutput {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            format: OutputFormat {
                device: "null".to_owned(),
                sample_rate,
                channels,
            },
        }
    }
}

/// The output thread stops when this sender is dropped.
struct StopOnDrop {
    _stop: std::sync::mpsc::Sender<()>,
}

impl AudioOutput for NullOutput {
    fn format(&self) -> OutputFormat {
        self.format.clone()
    }

    fn start(self: Box<Self>, mut renderer: Renderer) -> Result<Box<dyn Any>, String> {
        const PERIOD: Duration = Duration::from_millis(10);
        let rate = u64::from(self.format.sample_rate);
        let channels = usize::from(self.format.channels);
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            let started = Instant::now();
            let mut rendered = 0_u64;
            let mut buffer = vec![0.0_f32; 8192 * channels];
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(PERIOD)
            {
                let due = started.elapsed().as_millis() as u64 * rate / 1000;
                while rendered < due {
                    let frames = ((due - rendered) as usize).min(8192);
                    renderer.render(&mut buffer[..frames * channels]);
                    rendered += frames as u64;
                }
            }
        });
        Ok(Box::new(StopOnDrop { _stop: stop }))
    }
}
