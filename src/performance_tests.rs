//! 편곡·분할 성능과 순차·병렬 결과 일치를 측정합니다.

use std::{collections::BTreeMap, hint::black_box, path::PathBuf, time::Instant};

use anyhow::{Context, Result};

use crate::{
    Score, core,
    fold::{self, FoldOptions, Layout},
    input, salience,
    split::SplitOptions,
    verify, workflow,
};

/// 성능 측정용으로 악보를 시간순 반복합니다.
fn repeat_score(source: &Score, repeats: usize) -> Result<Score> {
    let original_total = source.total_ticks();
    let stride = (original_total / 384 + 2)
        .checked_mul(384)
        .context("benchmark repeat stride overflow")?;
    let last_offset = stride
        .checked_mul(repeats.saturating_sub(1) as i64)
        .context("benchmark repeat offset overflow")?;
    let total = original_total
        .checked_add(last_offset)
        .context("benchmark duration overflow")?;
    let mut source_tempos = BTreeMap::from([(0, 120)]);
    source_tempos.extend(source.tempos());
    let mut tempo_map = BTreeMap::new();
    for repeat in 0..repeats {
        let offset = stride * repeat as i64;
        for (&tick, &bpm) in &source_tempos {
            tempo_map.insert(
                tick.checked_add(offset)
                    .context("benchmark tempo offset overflow")?,
                bpm,
            );
        }
    }
    let tempos: Vec<_> = tempo_map.into_iter().collect();
    let mut result = Score {
        head: source
            .head
            .iter()
            .filter(|line| !line.starts_with("tempo="))
            .cloned()
            .collect(),
        tracks: Vec::with_capacity(source.tracks.len()),
        tail: source.tail.clone(),
    };
    result.head.push(format!(
        "tempo={}",
        tempos
            .iter()
            .map(|(tick, bpm)| format!("{tick}T{bpm}"))
            .collect::<Vec<_>>()
            .join(",")
    ));
    for (track_index, track) in source.tracks.iter().enumerate() {
        let mut texts = Vec::with_capacity(track.parts.len());
        for (part_index, part) in track.parts.iter().enumerate() {
            let mut notes = Vec::with_capacity(part.len() * repeats);
            for repeat in 0..repeats {
                let offset = stride * repeat as i64;
                notes.extend(part.iter().cloned().map(|mut note| {
                    note.on += offset;
                    note.off += offset;
                    note
                }));
            }
            texts.push(core::emit_part(
                &notes,
                if part_index == 0 { &tempos } else { &[] },
                total,
            )?);
        }
        result.tracks.push(core::track_from_mml(
            format!("MML@{};", texts.join(",")),
            track.meta.clone(),
            track_index,
        )?);
    }
    Ok(result)
}

/// 같은 작업을 세 번 측정해 중앙값을 구합니다.
fn median_three<T>(mut operation: impl FnMut() -> Result<T>) -> Result<(f64, T)> {
    let mut timings = [0.0_f64; 3];
    let mut last = None;
    for elapsed in &mut timings {
        let started = Instant::now();
        let value = black_box(operation()?);
        *elapsed = started.elapsed().as_secs_f64() * 1000.0;
        // 이전 결과의 해제 시간은 측정에서 제외합니다.
        last = Some(value);
    }
    timings.sort_by(f64::total_cmp);
    Ok((timings[1], last.expect("three benchmark samples")))
}

#[test]
#[ignore = "manual release-mode pipeline benchmark; no performance assertions"]
/// 입력·편곡·분할 단계의 실행 시간을 측정합니다.
fn benchmark_pipeline() -> Result<()> {
    let path = std::env::var_os("MMLFOLD_BENCH_INPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Op39No11_full_6part.mmi")
        });
    // 파일 읽기와 악보 반복 생성은 측정에서 제외합니다.
    let original = input::load_score(&path)?;
    let cases = [1, 4]
        .into_iter()
        .map(|repeats| repeat_score(&original, repeats).map(|score| (repeats, score)))
        .collect::<Result<Vec<_>>>()?;
    let options = FoldOptions {
        layout: Layout::Learned,
        ..FoldOptions::default()
    };
    let split_options = SplitOptions {
        limit: 2400,
        ..SplitOptions::default()
    };
    eprintln!("pipeline benchmark: {} (median of 3, ms)", path.display());
    for (repeats, score) in cases {
        let input_notes = score.all_notes().len();
        let (features_ms, rows) = median_three(|| Ok(salience::features(black_box(&score))))?;
        black_box(rows.len());
        drop(rows);
        let (probabilities_ms, probabilities) =
            median_three(|| Ok(salience::probabilities(black_box(&score))))?;
        black_box(probabilities.len());
        drop(probabilities);
        let (fold_ms, folded) = median_three(|| fold::fold_score(black_box(&score), &options))?;
        let (verify_ms, report) =
            median_three(|| Ok(verify::verify_scores(black_box(&score), &folded.score)))?;
        black_box(report.len());
        drop(report);
        let (scrolls_ms, scrolls) = median_three(|| {
            workflow::scrolls(black_box(&folded.score), &split_options, "benchmark")
        })?;
        let chunks = scrolls
            .split
            .as_ref()
            .map_or(0, |preview| preview.chunks.len());
        let core_fallback = folded.report.contains("core attacks: exact-tick fallback");
        eprintln!(
            "repeat={repeats} notes={input_notes} kept={} chunks={chunks} \
             features_ms={features_ms:.3} probabilities_including_features_ms={probabilities_ms:.3} \
             fold_ms={fold_ms:.3} verify_ms={verify_ms:.3} scrolls_ms={scrolls_ms:.3} \
             core_fallback={core_fallback}",
            folded.stats.kept,
        );
    }
    Ok(())
}

#[test]
/// 병렬 실행의 확률·MMI·분할 경계 일치를 검사합니다.
fn parallel_workers_preserve_probabilities_mmi_and_scroll_boundaries() -> Result<()> {
    let mut text = "[mml-score]\nversion=1\ntempo=0T120\n".to_owned();
    for track in 0..2 {
        let parts: Vec<_> = (0..3)
            .map(|part| format!("t120o{}l16{}", 5 - track - part, "cdefgab>c<".repeat(400)))
            .collect();
        text.push_str(&format!(
            "mml-track=MML@{};\nname=Track{}\nvisible=true\n",
            parts.join(","),
            track + 1
        ));
    }
    let score = core::parse_mmi(&text)?;
    assert_eq!(score.all_notes().len(), 19_200);
    let execute = || -> Result<_> {
        let probabilities = salience::probabilities(&score)
            .into_iter()
            .map(f64::to_bits)
            .collect::<Vec<_>>();
        let folded = fold::fold_score(
            &score,
            &FoldOptions {
                layout: Layout::Learned,
                ..FoldOptions::default()
            },
        )?;
        let scrolls = workflow::scrolls(&folded.score, &SplitOptions::default(), "parallel")?;
        let boundaries: Vec<_> = scrolls
            .split
            .as_ref()
            .unwrap()
            .chunks
            .iter()
            .map(|c| (c.start, c.end, c.part_widths.clone()))
            .collect();
        let artifacts: Vec<_> = scrolls
            .artifacts
            .into_iter()
            .map(|a| (a.name, a.text))
            .collect();
        Ok((
            probabilities,
            core::serialize_mmi(&folded.score),
            folded.report,
            folded.stats,
            boundaries,
            artifacts,
        ))
    };
    let serial = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()?
        .install(execute)?;
    for _ in 0..3 {
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()?
            .install(execute)?;
        assert_eq!(serial, parallel);
    }
    Ok(())
}
