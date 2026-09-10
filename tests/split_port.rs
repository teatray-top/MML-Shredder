//! 공통 시간축 분할의 음표 보존과 글자수 한도를 검사합니다.

use std::collections::BTreeMap;

use mmlfold::core::{emit_part, parse_mmi, track_from_mml};
use mmlfold::split::{SplitOptions, split_score};
use mmlfold::{Note, Score};

/// 음표나 MML 문자열로 테스트 악보를 만듭니다.
fn score(parts: &[&str]) -> Score {
    Score {
        head: vec!["[mml-score]".into(), "tempo=0T120".into()],
        tracks: parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                track_from_mml(
                    format!("MML@{part};"),
                    BTreeMap::from([("name".into(), format!("Piano {i}"))]),
                    i,
                )
                .unwrap()
            })
            .collect(),
        tail: vec!["[time-signature]".into(), "0=4/4".into()],
    }
}

/// 음높이별로 소리 나는 틱을 집계합니다.
fn sounding_ticks(notes: &[Note]) -> BTreeMap<(i64, i32, i32, usize, usize), usize> {
    let mut result = BTreeMap::new();
    for note in notes {
        for tick in note.on..note.off {
            *result
                .entry((tick, note.pitch, note.vel, note.src.0, note.src.1))
                .or_default() += 1;
        }
    }
    result
}

#[test]
/// 공통 절단의 틱 보존과 실제 문자열 길이를 검사합니다.
fn common_cuts_preserve_every_sounding_tick_and_obey_real_widths() {
    let upper = format!("t120{}", "c64d64e64f64g64a64b64>c64<".repeat(16));
    let lower = format!("t120o3{}", "c32e32g32c32".repeat(16));
    let source = score(&[&upper, &lower]);
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 35,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.chunks.len() > 1);
    let mut rebuilt = Vec::new();
    let mut previous = 0;
    for chunk in &result.chunks {
        assert_eq!(chunk.start, previous);
        assert!(chunk.end > chunk.start);
        assert_eq!(chunk.score.tracks.len(), 2);
        assert!(chunk.widths.iter().all(|w| *w <= 35));
        for part in chunk.mml_parts.iter().flatten() {
            assert!(part.chars().count() <= 35);
        }
        assert_eq!(chunk.score.tracks[0].meta["name"], "Piano 0");
        for mut note in chunk.score.all_notes() {
            note.on += chunk.start;
            note.off += chunk.start;
            rebuilt.push(note);
        }
        previous = chunk.end;
    }
    assert_eq!(previous, source.total_ticks());
    assert_eq!(
        sounding_ticks(&rebuilt),
        sounding_ticks(&source.all_notes())
    );
}

#[test]
/// 엄격 분할에서 지속음 조각이 사라지지 않는지 검사합니다.
fn held_note_fragments_never_disappear() {
    let source = score(&["t120c1&c1&c1&c1&c1&c1&c1&c1"]);
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 9,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.chunks.len() > 1);
    let mut duration = 0;
    for chunk in &result.chunks {
        for note in chunk.score.all_notes() {
            assert!(note.duration() >= 6);
            duration += note.duration();
        }
    }
    assert_eq!(duration, source.total_ticks());
}

#[test]
/// 각 장의 템포·박자표 시각 재설정을 검사합니다.
fn local_tempo_and_time_signature_are_rebased() {
    let mut source = score(&["t120c4d4t150e4f4g4a4t90b4>c4"]);
    source.tail = vec![
        "[time-signature]".into(),
        "0=4/4".into(),
        "192=3/4".into(),
        "576=6/8".into(),
        "[marker]".into(),
        "title=Preserve me".into(),
    ];
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.chunks.len() > 1);
    for chunk in result.chunks {
        let expected_tempo = source
            .tempos()
            .into_iter()
            .rev()
            .find(|(t, _)| *t <= chunk.start)
            .unwrap()
            .1;
        assert_eq!(chunk.score.tracks[0].tempos[0], (0, expected_tempo));
        assert!(
            chunk
                .score
                .head
                .iter()
                .any(|line| line.starts_with(&format!("tempo=0T{expected_tempo}")))
        );
        let signature = if chunk.start >= 576 {
            "0=6/8"
        } else if chunk.start >= 192 {
            "0=3/4"
        } else {
            "0=4/4"
        };
        assert!(chunk.score.tail.iter().any(|line| line == signature));
        assert!(
            chunk
                .score
                .tail
                .iter()
                .any(|line| line == "title=Preserve me")
        );
        assert!(
            chunk.score.tracks[0]
                .tempos
                .iter()
                .all(|(t, _)| *t >= 0 && *t < chunk.end - chunk.start)
        );
    }
}

#[test]
/// 공통 절단이 전체 쉼표 길이를 줄이지 않는지 검사합니다.
fn cuts_never_shorten_a_global_rest() {
    let source = score(&["t120c4d4e4f4r1r1g4a4b4>c4"]);
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 11,
            ..Default::default()
        },
    )
    .unwrap();
    for chunk in &result.chunks {
        assert!(chunk.end <= 384 || chunk.end > 1152);
        assert_eq!(chunk.score.total_ticks(), chunk.end - chunk.start);
    }
}

#[test]
/// 작은 글자수 한도에서 비단조적인 인코딩 길이를 검사합니다.
fn small_limits_check_nonmonotonic_note_lengths() {
    let source = score(&["t120c1"]);
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 5,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.chunks.len(), 4);
    assert!(
        result
            .chunks
            .iter()
            .all(|chunk| chunk.end - chunk.start == 96)
    );
}

#[test]
/// 마지막 짧은 조각 때문에 이전 절단점을 재계산하는지 검사합니다.
fn a_short_final_fragment_replans_the_previous_cut() {
    let source = score(&["t120c7&c8"]); // 54 + 48 = 102 ticks.
    let result = split_score(
        &source,
        &SplitOptions {
            limit: 6,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.chunks.len(), 2);
    assert_eq!(
        result
            .chunks
            .iter()
            .map(|chunk| chunk.end - chunk.start)
            .sum::<i64>(),
        102
    );
    assert!(
        result
            .chunks
            .iter()
            .flat_map(|chunk| &chunk.widths)
            .all(|width| *width <= 6)
    );
}

#[test]
/// 짧은 구간의 분할 계획을 독립 도달 가능성 계산과 대조합니다.
fn small_duration_plans_match_an_independent_reachability_check() {
    const MAX_DURATION: usize = 180;
    let mut texts = vec![String::new(); MAX_DURATION + 1];
    for (duration, text) in texts.iter_mut().enumerate().skip(6) {
        *text = emit_part(
            &[Note {
                on: 0,
                off: duration as i64,
                pitch: 60,
                vel: 8,
                src: (0, 0),
            }],
            &[(0, 120)],
            duration as i64,
        )
        .unwrap();
    }
    for limit in 5..=7 {
        let allowed: Vec<_> = (6..=MAX_DURATION)
            .filter(|duration| texts[*duration].len() <= limit)
            .collect();
        let mut reachable = [false; MAX_DURATION + 1];
        reachable[0] = true;
        for total in 6..=MAX_DURATION {
            reachable[total] = allowed
                .iter()
                .any(|duration| *duration <= total && reachable[total - duration]);
            let source = score(&[&texts[total]]);
            let result = split_score(
                &source,
                &SplitOptions {
                    limit,
                    ..Default::default()
                },
            );
            assert_eq!(
                result.is_ok(),
                reachable[total],
                "duration={total}, limit={limit}, error={:?}",
                result.err()
            );
        }
    }
}

#[test]
/// 빈 입력·잘못된 입력·불가능한 분할을 명확히 처리하는지 검사합니다.
fn empty_invalid_and_impossible_inputs_are_explicit() {
    assert!(
        split_score(&Score::default(), &SplitOptions::default())
            .unwrap()
            .chunks
            .is_empty()
    );
    assert!(
        split_score(
            &score(&["c4"]),
            &SplitOptions {
                limit: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        split_score(
            &score(&["c4"]),
            &SplitOptions {
                limit: 4,
                ..Default::default()
            }
        )
        .is_err()
    );
    let mut invalid = score(&["c4"]);
    invalid.tracks[0].parts[0][0].off = 3;
    assert!(split_score(&invalid, &SplitOptions::default()).is_err());
}

#[test]
/// 실제 합주 악보의 글자수와 전체 시간 범위를 검사합니다.
fn supplied_ensemble_fits_and_covers_original_duration() {
    let source = parse_mmi(include_str!("../Op39No11_full_6part.mmi")).unwrap();
    let result = split_score(&source, &SplitOptions::default()).unwrap();
    assert!(!result.chunks.is_empty());
    assert_eq!(result.chunks.last().unwrap().end, source.total_ticks());
    assert!(
        result
            .chunks
            .iter()
            .flat_map(|chunk| &chunk.widths)
            .all(|width| *width <= 2400)
    );
    let before: i64 = source.all_notes().iter().map(Note::duration).sum();
    let after: i64 = result
        .chunks
        .iter()
        .flat_map(|chunk| chunk.score.all_notes())
        .map(|note| note.duration())
        .sum();
    assert_eq!(before, after);
}
