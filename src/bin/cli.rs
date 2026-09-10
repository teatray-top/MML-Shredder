//! 명령행에서 MIDI 변환·편곡·분할·검증 작업을 실행합니다.

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mmlfold::{
    core,
    fold::{self, FoldOptions, Gain, Layout, VolumeRange},
    input,
    split::SplitOptions,
    verify, workflow,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "MML 세단기 CLI", version, about = "MIDI·MMI 편곡과 악보 분할")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum LayoutArg {
    Voices,
    Hands,
    Roles,
    Learned,
}

/// 최소:최대 형식의 음량 범위를 읽고 검증합니다.
fn parse_volume_range(value: &str) -> std::result::Result<VolumeRange, String> {
    const HELP: &str = "1..15 범위의 최소:최대를 입력하세요 (예: 11:15, 최소는 최대 이하).";
    let (min, max) = value.split_once(':').ok_or_else(|| HELP.to_owned())?;
    let min = min.parse::<i32>().map_err(|_| HELP.to_owned())?;
    let max = max.parse::<i32>().map_err(|_| HELP.to_owned())?;
    if !(1..=15).contains(&min) || !(min..=15).contains(&max) {
        return Err(HELP.to_owned());
    }
    Ok(VolumeRange { min, max })
}

#[derive(Subcommand)]
enum Command {
    /// MIDI를 성부 축소 없이 MMI로 변환합니다.
    Import {
        src: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// 성부를 줄이고 트랙을 다시 배치합니다.
    Fold {
        src: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value_t = 2)]
        tracks: usize,
        #[arg(long, default_value_t = 3)]
        parts: usize,
        #[arg(long, value_enum, default_value = "hands")]
        layout: LayoutArg,
        #[arg(long, default_value = "auto", allow_hyphen_values = true)]
        gain: String,
        /// gain 적용 후 양수 음량을 출력 범위로 압축합니다 (예: 11:15).
        #[arg(long, value_name = "MIN:MAX", value_parser = parse_volume_range, allow_hyphen_values = true)]
        volume_range: Option<VolumeRange>,
        #[arg(long, default_value_t = 0.6)]
        lead_pitch: f64,
        #[arg(long, default_value_t = 0.7)]
        lead_continuity: f64,
        #[arg(long)]
        track_files: bool,
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// 모든 트랙을 공통 지점에서 글자수 한도에 맞게 분할합니다.
    Split {
        src: PathBuf,
        #[arg(short = 'd', long)]
        outdir: PathBuf,
        #[arg(long, default_value_t = 2400)]
        limit: usize,
        #[arg(long, default_value_t = 24)]
        min_gap: i64,
        #[arg(long, default_value_t = 96)]
        big_gap: i64,
        #[arg(long)]
        force: bool,
    },
    /// 두 악보의 음표, 동시발음 수, 템포를 비교합니다.
    Verify { src: PathBuf, dst: PathBuf },
    /// 악보의 기본 정보를 표시합니다.
    Inspect { src: PathBuf },
}

/// 명령행 설정에 따라 악보 작업과 파일 저장을 실행합니다.
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Import { src, out, force } => {
            let score = input::load_score(&src)?;
            workflow::write_files(&[(out.clone(), core::serialize_mmi(&score))], force, &[src])?;
            println!("Saved: {}", out.display());
        }
        Command::Fold {
            src,
            out,
            tracks,
            parts,
            layout,
            gain,
            volume_range,
            lead_pitch,
            lead_continuity,
            track_files,
            report,
            force,
        } => {
            let score = input::load_score(&src)?;
            let gain =
                match gain.as_str() {
                    "auto" => Gain::Auto,
                    "none" => Gain::None,
                    _ => Gain::Shift(gain.parse().map_err(|_| {
                        anyhow::anyhow!("--gain: auto, none 또는 정수를 사용하세요")
                    })?),
                };
            let layout = match layout {
                LayoutArg::Voices => Layout::Voices,
                LayoutArg::Hands => Layout::Hands,
                LayoutArg::Roles => Layout::Roles,
                LayoutArg::Learned => Layout::Learned,
            };
            let result = fold::fold_score(
                &score,
                &FoldOptions {
                    tracks,
                    parts,
                    layout,
                    gain,
                    volume_range,
                    lead_pitch,
                    lead_continuity,
                },
            )?;
            let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("score");
            let report_text = result.report;
            let mut files = vec![(out.clone(), core::serialize_mmi(&result.score))];
            if track_files {
                let ext = out.extension().and_then(|s| s.to_str()).unwrap_or("mmi");
                for (i, track) in result.score.tracks.iter().enumerate() {
                    let mut single = result.score.clone();
                    single.tracks = vec![track.clone()];
                    single.tracks[0].meta.insert("name".into(), "Track1".into());
                    files.push((
                        out.with_file_name(format!("{stem}_t{}.{ext}", i + 1)),
                        core::serialize_mmi(&single),
                    ));
                }
            }
            if let Some(path) = report {
                files.push((path, report_text.clone()));
            }
            workflow::write_files(&files, force, &[src])?;
            println!("{}\nSaved: {}", report_text, out.display());
        }
        Command::Split {
            src,
            outdir,
            limit,
            min_gap,
            big_gap,
            force,
        } => {
            let score = input::load_score(&src)?;
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("score");
            let result = workflow::scrolls(
                &score,
                &SplitOptions {
                    limit,
                    min_gap,
                    big_gap,
                },
                stem,
            )?;
            if result.artifacts.is_empty() {
                bail!("분할 결과가 없습니다");
            }
            let files = result
                .artifacts
                .iter()
                .map(|a| (outdir.join(&a.name), a.text.clone()))
                .collect::<Vec<_>>();
            workflow::write_files(&files, force, &[src])?;
            println!("{}\n\nSaved: {}", result.report, outdir.display());
        }
        Command::Verify { src, dst } => println!(
            "{}",
            verify::verify_scores(&input::load_score(&src)?, &input::load_score(&dst)?)
        ),
        Command::Inspect { src } => {
            let score = input::load_score(&src)?;
            let notes = score.all_notes();
            println!(
                "tracks: {}\nparts: {}\nnotes: {}\nticks: {}\nmax polyphony: {}\ntempo events: {}",
                score.tracks.len(),
                score.tracks.iter().map(|t| t.parts.len()).sum::<usize>(),
                notes.len(),
                score.total_ticks(),
                verify::max_polyphony(&notes),
                score.tempos().len()
            );
        }
    }
    Ok(())
}
