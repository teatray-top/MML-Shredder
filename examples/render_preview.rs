//! GUI와 같은 재생기로 악보의 일부를 모노 WAV에 저장합니다.

use anyhow::{Context, Result, ensure};
use clap::Parser;
use mmlfold::{
    core,
    synth::{Renderer, Timeline},
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
struct Options {
    input: PathBuf,
    output: PathBuf,
    #[arg(long, default_value_t = 15.0)]
    seconds: f64,
}

/// 악보의 지정 구간을 모노 WAV 파일로 저장합니다.
fn main() -> Result<()> {
    let options = Options::parse();
    ensure!(
        options.seconds.is_finite() && (0.0..=600.0).contains(&options.seconds),
        "Duration must be between 0 and 600 seconds"
    );
    ensure!(
        options.input != options.output,
        "Output must not replace the input score"
    );
    let source = core::load_mmi(&options.input)?;
    let timeline = Arc::new(Timeline::from_score(&source)?);
    const SAMPLE_RATE: u32 = 48_000;
    let frames =
        (options.seconds.min(timeline.duration_seconds()) * f64::from(SAMPLE_RATE)).ceil() as u32;
    let mut renderer = Renderer::new(timeline, SAMPLE_RATE);
    let mut wav = BufWriter::new(
        File::create_new(&options.output).context("Cannot create a new preview WAV")?,
    );
    let data_bytes = frames * 2;
    wav.write_all(b"RIFF")?;
    wav.write_all(&(36 + data_bytes).to_le_bytes())?;
    wav.write_all(b"WAVEfmt ")?;
    wav.write_all(&16_u32.to_le_bytes())?;
    wav.write_all(&1_u16.to_le_bytes())?; // PCM
    wav.write_all(&1_u16.to_le_bytes())?; // mono
    wav.write_all(&SAMPLE_RATE.to_le_bytes())?;
    wav.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?;
    wav.write_all(&2_u16.to_le_bytes())?;
    wav.write_all(&16_u16.to_le_bytes())?;
    wav.write_all(b"data")?;
    wav.write_all(&data_bytes.to_le_bytes())?;
    for _ in 0..frames {
        // 좌우가 같은 모노 신호이므로 한 채널만 저장합니다.
        let sample = renderer.next_frame()[0];
        // 정규화하지 않고 GUI 기본 음량을 적용합니다.
        let sample = (sample * 0.65 * f32::from(i16::MAX)).round() as i16;
        wav.write_all(&sample.to_le_bytes())?;
    }
    wav.flush()?;
    println!(
        "Rendered {:.3} s of sampled piano to {}",
        f64::from(frames) / f64::from(SAMPLE_RATE),
        options.output.display()
    );
    Ok(())
}
