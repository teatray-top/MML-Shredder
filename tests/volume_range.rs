//! DR 압축의 음량 변환과 편곡·재생 보존을 검사합니다.

use mmlfold::{
    Note, Score, Track, core,
    fold::{FoldOptions, Gain, Layout, VolumeRange, fold_score},
    split::SplitOptions,
    synth::Timeline,
    workflow,
};

const COMPRESSED: [i32; 16] = [
    0, 11, 11, 12, 12, 12, 12, 13, 13, 13, 14, 14, 14, 14, 15, 15,
];

/// DR 압축을 검사할 원본 악보를 만듭니다.
fn source(volumes: &[i32], voices: usize) -> Score {
    Score {
        head: vec!["[mml-score]".into(), "title=Volume range".into()],
        tracks: (0..voices)
            .map(|voice| Track {
                parts: vec![
                    volumes
                        .iter()
                        .enumerate()
                        .map(|(i, &vel)| Note {
                            on: i as i64 * 96,
                            off: (i as i64 + 1) * 96,
                            pitch: 36 + voice as i32 * 7 + (i % 3) as i32,
                            vel: (vel + voice as i32).min(15),
                            src: (voice, 0),
                        })
                        .collect(),
                ],
                tempos: vec![(0, 120), (192, 144)],
                ..Track::default()
            })
            .collect(),
        tail: vec!["[time-signature]".into(), "0=4/4".into()],
    }
}

/// 지정한 음량 범위로 편곡 옵션을 만듭니다.
fn options() -> FoldOptions {
    FoldOptions {
        gain: Gain::None,
        volume_range: Some(VolumeRange { min: 11, max: 15 }),
        ..FoldOptions::default()
    }
}

/// 악보의 음량을 순서대로 모읍니다.
fn volumes(score: &Score) -> Vec<i32> {
    let mut notes = score.all_notes();
    notes.sort_by_key(|note| (note.on, note.pitch));
    notes.into_iter().map(|note| note.vel).collect()
}

#[test]
/// 전체 음량 범위의 압축과 저장·미리듣기 반영을 검사합니다.
fn full_volume_scale_is_compressed_and_survives_export_and_preview() {
    let input = source(&(0..=15).collect::<Vec<_>>(), 1);
    let original = input.all_notes();
    let result = workflow::arrange(&input, &options(), "dynamics", false).unwrap();
    assert_eq!(volumes(&result.score), COMPRESSED);
    assert_eq!(input.all_notes(), original);
    assert_eq!(result.score.tempos(), input.tempos());
    assert_eq!(result.artifacts.len(), 1);
    let restored = core::parse_mmi(&result.artifacts[0].text).unwrap();
    assert_eq!(restored.all_notes(), result.score.all_notes());
    let preview = Timeline::from_score(&restored).unwrap();
    for (note, &volume) in preview.notes().iter().zip(&COMPRESSED) {
        assert!((note.velocity - volume as f32 / 15.0).abs() < f32::EPSILON);
    }
    let split = workflow::scrolls(&restored, &SplitOptions::default(), "dynamics").unwrap();
    assert_eq!(split.artifacts.len(), 1);
    let chunk = core::parse_mmi(&split.artifacts[0].text).unwrap();
    assert_eq!(chunk.all_notes(), restored.all_notes());
    assert_eq!(chunk.tempos(), restored.tempos());
}

#[test]
/// gain 뒤에 압축하고 무음으로 제한된 음을 살리지 않는지 검사합니다.
fn compression_follows_gain_and_does_not_revive_shifted_silence() {
    for (input, gain, expected, shift) in [
        ([0, 1, 8, 15], Gain::None, [0, 11, 13, 15], 0),
        ([0, 1, 8, 15], Gain::Shift(2), [11, 12, 14, 15], 2),
        ([0, 1, 8, 15], Gain::Shift(-2), [0, 0, 12, 14], -2),
        ([0, 1, 8, 10], Gain::Auto, [12, 12, 14, 15], 5),
        ([0, 1, 8, 15], Gain::Shift(i32::MIN), [0; 4], i32::MIN),
    ] {
        let result = fold_score(&source(&input, 1), &FoldOptions { gain, ..options() }).unwrap();
        assert_eq!(volumes(&result.score), expected, "gain={gain:?}");
        assert_eq!(result.stats.volume_shift, shift);
        assert!(!result.report.contains("dynamic range unchanged"));
    }
}

#[test]
/// 모든 배치에서 DR 압축이 축소·파트 선택을 바꾸지 않는지 검사합니다.
fn volume_range_does_not_change_reduction_or_part_assignment_in_any_layout() {
    let input = source(&[1, 15, 4, 12, 2, 14, 8, 3], 8);
    let original = input.all_notes();
    for layout in [
        Layout::Voices,
        Layout::Hands,
        Layout::Roles,
        Layout::Learned,
    ] {
        for gain in [Gain::None, Gain::Auto, Gain::Shift(-3)] {
            let base = FoldOptions {
                layout,
                gain,
                ..FoldOptions::default()
            };
            let baseline = fold_score(&input, &base).unwrap();
            for range in [
                VolumeRange { min: 1, max: 15 },
                VolumeRange { min: 11, max: 15 },
                VolumeRange { min: 11, max: 11 },
            ] {
                let result = fold_score(
                    &input,
                    &FoldOptions {
                        volume_range: Some(range),
                        ..base.clone()
                    },
                )
                .unwrap();
                assert_eq!(result.stats, baseline.stats);
                assert_eq!(result.score.tempos(), baseline.score.tempos());
                for (old, new) in baseline
                    .score
                    .all_notes()
                    .iter()
                    .zip(result.score.all_notes())
                {
                    assert_eq!(
                        (new.on, new.off, new.pitch, new.src),
                        (old.on, old.off, old.pitch, old.src),
                        "layout={layout:?}, gain={gain:?}, range={range:?}"
                    );
                    let expected = if old.vel == 0 || range.min == 1 {
                        old.vel
                    } else if range.min == range.max {
                        11
                    } else {
                        COMPRESSED[old.vel as usize]
                    };
                    assert_eq!(new.vel, expected);
                }
                if range.min == 1 {
                    assert_eq!(
                        core::serialize_mmi(&result.score),
                        core::serialize_mmi(&baseline.score)
                    );
                }
            }
        }
    }
    assert_eq!(input.all_notes(), original);
}

#[test]
/// 정확한 시간 보존을 위한 재시도에서도 압축을 한 번만 적용하는지 검사합니다.
fn compression_is_applied_once_after_exact_timing_fallback() {
    let input = Score {
        tracks: vec![
            Track {
                parts: vec![vec![Note {
                    on: 0,
                    off: 192,
                    pitch: 84,
                    vel: 8,
                    src: (0, 0),
                }]],
                tempos: vec![(0, 120), (97, 144)],
                ..Track::default()
            },
            Track {
                parts: vec![vec![Note {
                    on: 96,
                    off: 192,
                    pitch: 72,
                    vel: 1,
                    src: (1, 0),
                }]],
                ..Track::default()
            },
        ],
        ..Score::default()
    };
    let settings = FoldOptions {
        tracks: 1,
        parts: 1,
        layout: Layout::Learned,
        ..options()
    };
    let result = fold_score(&input, &settings).unwrap();
    assert!(result.report.contains("original interval priorities"));
    assert_eq!(volumes(&result.score), [13]);
    assert_eq!(result.score.tempos(), input.tempos());
    let note = &result.score.tracks[0].parts[0][0];
    assert_eq!((note.on, note.off, note.pitch), (0, 192, 84));
    let restored = core::parse_mmi(&core::serialize_mmi(&result.score)).unwrap();
    assert_eq!(restored.all_notes(), result.score.all_notes());
}

#[test]
/// 빈 악보의 범위 검증과 동일 음량 출력을 검사합니다.
fn volume_range_validates_empty_scores_and_handles_constant_volumes() {
    for (min, max) in [(0, 15), (1, 16), (12, 11), (i32::MIN, 15), (1, i32::MAX)] {
        let result = fold_score(
            &Score::default(),
            &FoldOptions {
                volume_range: Some(VolumeRange { min, max }),
                ..options()
            },
        );
        assert!(result.unwrap_err().to_string().contains("Volume range"));
    }
    assert!(
        fold_score(&Score::default(), &options())
            .unwrap()
            .score
            .all_notes()
            .is_empty()
    );
    assert_eq!(
        volumes(&fold_score(&source(&[8; 4], 1), &options()).unwrap().score),
        [13; 4]
    );
    assert_eq!(
        volumes(&fold_score(&source(&[0; 4], 1), &options()).unwrap().score),
        [0; 4]
    );
    assert_eq!(FoldOptions::default().volume_range, None);
}
