//! Audio capture: cpal input device → ring buffer of interleaved stereo `f32`, plus a
//! built-in synthetic signal (`--input test`) so rendering can be checked without a sound card.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use rust_i18n::t;

/// Seconds of audio kept in the ring buffer.
const RING_SECONDS: usize = 1;

/// A capture source. Samples are interleaved stereo, `-1.0..=1.0` before gain.
pub struct Capture {
    _stream: Option<cpal::Stream>,
    ring: Arc<Mutex<VecDeque<f32>>>,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Multiplier applied to every returned sample.
    pub gain: f32,
    test: Option<Instant>,
}

impl Capture {
    /// Names of the input devices, with `true` for the system default.
    pub fn list() -> Result<Vec<(String, bool)>> {
        let host = cpal::default_host();
        let default = host
            .default_input_device()
            .and_then(|d| device_name(&d).ok());
        host.input_devices()?
            .map(|d| device_name(&d).map(|n| (n.clone(), Some(n) == default)))
            .collect()
    }

    /// Opens `selector` (index or case-insensitive name substring), or the default input.
    pub fn open(selector: Option<&str>, gain: f32) -> Result<Self> {
        let host = cpal::default_host();
        let device = match selector {
            None => host.default_input_device(),
            Some(sel) => {
                let devices: Vec<_> = host.input_devices()?.collect();
                match sel.parse::<usize>() {
                    Ok(i) => devices.into_iter().nth(i),
                    Err(_) => devices.into_iter().find(|d| {
                        device_name(d).is_ok_and(|n| n.to_lowercase().contains(&sel.to_lowercase()))
                    }),
                }
            }
        }
        .ok_or_else(|| match selector {
            Some(name) => anyhow!(t!("audio.device_not_found", name = name)),
            None => anyhow!(t!("audio.no_devices")),
        })?;
        let config = device.default_input_config()?;
        let rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let ring = Arc::new(Mutex::new(VecDeque::with_capacity(
            rate as usize * 2 * RING_SECONDS,
        )));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config.into(), channels, &ring),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config.into(), channels, &ring),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config.into(), channels, &ring),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config.into(), channels, &ring),
            cpal::SampleFormat::U8 => build::<u8>(&device, &config.into(), channels, &ring),
            other => Err(anyhow!("unsupported sample format {other:?}")),
        }?;
        stream.play()?;
        eprintln!(
            "{}",
            t!(
                "audio.using_device",
                name = device_name(&device)?,
                rate = rate,
                channels = channels
            )
        );
        Ok(Self {
            _stream: Some(stream),
            ring,
            rate,
            gain,
            test: None,
        })
    }

    /// A deterministic synthetic signal: a kick every 500 ms plus a slow sine sweep.
    pub fn test(gain: f32) -> Self {
        Self {
            _stream: None,
            ring: Arc::new(Mutex::new(VecDeque::new())),
            rate: 44_100,
            gain,
            test: Some(Instant::now()),
        }
    }

    /// The most recent `frames` stereo frames (interleaved, gain applied), zero-padded at the
    /// front when fewer have been captured.
    pub fn latest(&self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        if let Some(start) = self.test {
            let t_end = start.elapsed().as_secs_f64();
            let dt = 1.0 / f64::from(self.rate);
            for (i, frame) in out.chunks_exact_mut(2).enumerate() {
                let t = t_end - (frames - i) as f64 * dt;
                let v = test_signal(t) as f32 * self.gain;
                frame[0] = v;
                frame[1] = v * 0.7;
            }
            return out;
        }
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let available = ring.len().min(out.len());
        let (a, b) = ring.as_slices();
        let all = [a, b].concat();
        let src = &all[all.len() - available..];
        let skip = out.len() - available;
        let dst = &mut out[skip..];
        for (d, s) in dst.iter_mut().zip(src) {
            *d = s * self.gain;
        }
        out
    }
}

fn device_name(device: &cpal::Device) -> Result<String> {
    Ok(device.description()?.name().to_owned())
}

/// Builds an input stream that downmixes `channels` to stereo `f32` into `ring`.
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    ring: &Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ring = Arc::clone(ring);
    let cap = config.sample_rate as usize * 2 * RING_SECONDS;
    let stream = device.build_input_stream::<T, _, _>(
        *config,
        move |data, _| {
            let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
            for frame in data.chunks_exact(channels) {
                let l = f32::from_sample_(frame[0]);
                let r = f32::from_sample_(frame[1.min(channels - 1)]);
                ring.push_back(l);
                ring.push_back(r);
            }
            let excess = ring.len().saturating_sub(cap);
            ring.drain(..excess);
        },
        |e| log::error!("{}", t!("audio.stream_error", error = e)),
        None,
    )?;
    Ok(stream)
}

/// Kick drum (decaying 55 Hz) every half second, over a quiet 200-4000 Hz sweep.
fn test_signal(t: f64) -> f64 {
    use std::f64::consts::TAU;
    let beat = t.rem_euclid(0.5);
    let kick = (-beat * 12.0).exp() * (TAU * 55.0 * beat).sin();
    let sweep_hz = 200.0 + 3800.0 * (0.5 + 0.5 * (t * 0.3).sin());
    kick * 0.8 + 0.15 * (TAU * sweep_hz * t).sin()
}

/// Prints the device list to stdout.
pub fn print_devices() -> Result<()> {
    let devices = Capture::list().context(t!("audio.no_devices"))?;
    println!("{}", t!("audio.devices_header"));
    for (i, (name, is_default)) in devices.iter().enumerate() {
        println!("  {i}: {name}{}", if *is_default { " *" } else { "" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_is_bounded_and_has_kicks() {
        let peak = (0..44_100)
            .map(|i| test_signal(f64::from(i) / 44_100.0).abs())
            .fold(0.0, f64::max);
        assert!(peak <= 1.0 && peak > 0.5, "peak {peak}");
        assert!(test_signal(0.002).abs() > test_signal(0.45).abs());
    }

    #[test]
    fn latest_pads_with_zeros_at_the_front() {
        let mut c = Capture::test(1.0);
        c.test = None;
        c.ring.lock().unwrap().extend([0.5, -0.5]);
        assert_eq!(c.latest(2), vec![0.0, 0.0, 0.5, -0.5]);
    }
}
