//! 화음 압축이 중복음보다 화음의 성격과 실제 외성을 보존하는지 검사합니다.

use mmlfold::core::{parse_mmi, serialize_mmi};
use mmlfold::fold::{FoldOptions, Gain, Layout, fold_score};
use mmlfold::{Note, Score, Track};

/// 같은 시각에 시작하는 독립 성부로 테스트 화음을 만듭니다.
fn chord(pitches: &[i32]) -> Score {
    Score {
        tracks: pitches
            .iter()
            .enumerate()
            .map(|(track, &pitch)| Track {
                parts: vec![vec![Note {
                    on: 0,
                    off: 96,
                    pitch,
                    vel: 10,
                    src: (track, 0),
                }]],
                ..Track::default()
            })
            .collect(),
        ..Score::default()
    }
}

/// 모든 배치 방식과 이조에서 선택된 음과 MMI의 정확한 음가를 검사합니다.
fn assert_reduction(pitches: &[i32], expected: &[i32], tracks: usize, parts: usize) {
    for layout in [
        Layout::Voices,
        Layout::Hands,
        Layout::Roles,
        Layout::Learned,
    ] {
        for shift in -6..=6 {
            for reverse in [false, true] {
                let mut pitches: Vec<_> = pitches.iter().map(|p| p + shift).collect();
                if reverse {
                    pitches.reverse();
                }
                let source = chord(&pitches);
                let result = fold_score(
                    &source,
                    &FoldOptions {
                        tracks,
                        parts,
                        layout,
                        gain: Gain::None,
                        ..FoldOptions::default()
                    },
                )
                .unwrap();
                let reloaded = parse_mmi(&serialize_mmi(&result.score)).unwrap();
                let mut actual: Vec<_> = reloaded
                    .all_notes()
                    .iter()
                    .map(|n| (n.on, n.off, n.pitch - shift, n.vel))
                    .collect();
                actual.sort_unstable();
                let mut expected: Vec<_> = expected.iter().map(|&p| (0, 96, p, 10)).collect();
                expected.sort_unstable();
                assert_eq!(
                    actual, expected,
                    "{layout:?}, shift={shift}, reverse={reverse}"
                );
                assert_eq!(result.stats.truncated, 0);
                assert_eq!(result.stats.kept, expected.len());
                assert_eq!(source.all_notes().len(), pitches.len());
            }
        }
    }
}

#[test]
/// 중복 근음과 완전5도보다 유일한 3음과 외성을 남깁니다.
fn compression_keeps_third_with_outer_voices() {
    assert_reduction(&[36, 60, 64, 67, 84], &[36, 64, 84], 1, 3);
    assert_reduction(&[36, 60, 63, 67, 84], &[36, 63, 84], 1, 3);
}

#[test]
/// 7화음 압축에서 3음과 7음을 모두 남깁니다.
fn compression_keeps_third_and_seventh() {
    assert_reduction(&[36, 60, 64, 67, 70, 84], &[36, 64, 70, 84], 2, 2);
    assert_reduction(&[36, 60, 64, 67, 71, 84], &[36, 64, 71, 84], 2, 2);
}

#[test]
/// 감화음의 변형된 5도를 완전5도처럼 제거하지 않습니다.
fn compression_keeps_altered_fifth() {
    assert_reduction(&[36, 60, 63, 66, 84], &[36, 63, 66, 84], 2, 2);
}

#[test]
/// 전위 화음의 실제 베이스를 근음으로 대체하지 않습니다.
fn compression_preserves_inverted_bass() {
    assert_reduction(&[40, 60, 64, 67, 84], &[40, 67, 84], 1, 3);
}

#[test]
/// 여유가 있는 화음에서는 중복음도 그대로 남깁니다.
fn under_capacity_keeps_all_chord_notes() {
    assert_reduction(&[36, 60, 64, 67, 70, 84], &[36, 60, 64, 67, 70, 84], 2, 3);
}
