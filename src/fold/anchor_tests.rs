//! 엇갈려 시작하는 반주가 다른 성부의 선율 연결 기준을 바꾸지 않는지 검사합니다.

use super::*;
use crate::Track;

/// 독립 성부의 공격을 분석 가능한 악보로 만듭니다.
fn source(voices: &[&[(Tick, Tick, i32)]]) -> Score {
    Score {
        tracks: voices
            .iter()
            .enumerate()
            .map(|(track, voice)| Track {
                parts: vec![
                    voice
                        .iter()
                        .map(|&(on, off, pitch)| Note {
                            on,
                            off,
                            pitch,
                            vel: 12,
                            src: (track, 0),
                        })
                        .collect(),
                ],
                ..Track::default()
            })
            .collect(),
        ..Score::default()
    }
}

/// 각 시점에 선택된 주선율 후보의 음높이를 반환합니다.
fn anchors(score: &Score) -> Vec<(Tick, i32)> {
    let notes = analyze(score).unwrap();
    let originals: Vec<_> = notes.iter().map(|n| n.note.clone()).collect();
    let kept: Vec<_> = (0..notes.len()).collect();
    let selected = attack_anchors(&notes, &originals, &kept, &FoldOptions::default());
    let mut result: Vec<_> = notes
        .iter()
        .zip(selected)
        .filter(|(_, selected)| *selected)
        .map(|(n, _)| (n.note.on, n.note.pitch))
        .collect();
    result.sort_unstable();
    result
}

#[test]
/// 두 옥타브 선율 사이에 반주가 시작해도 위 선율의 연결 기준은 유지됩니다.
fn staggered_accompaniment_does_not_pull_the_anchor_down_an_octave() {
    let melody = &[(0, 48, 96), (48, 96, 91), (96, 144, 88)][..];
    let octave = &[(0, 48, 84), (48, 96, 79), (96, 144, 76)][..];
    let accompaniment = &[(30, 48, 60), (78, 96, 58)][..];
    for shift in -12..=12 {
        let mut score = source(&[melody, octave, accompaniment]);
        for track in &mut score.tracks {
            for n in &mut track.parts[0] {
                n.pitch += shift;
            }
        }
        let selected = anchors(&score);
        for (on, pitch) in [(0, 96), (48, 91), (96, 88)] {
            assert!(
                selected.contains(&(on, pitch + shift)),
                "shift={shift}: {selected:?}"
            );
        }
    }
}

#[test]
/// 더 강한 새 성부와 긴 쉼 뒤 돌아온 성부가 선율 후보가 될 수 있습니다.
fn new_source_and_new_phrase_can_take_the_melody() {
    let mut score = source(&[
        &[(0, 48, 72), (48, 96, 71)],
        &[(48, 96, 84)],
        &[(0, 48, 40), (240, 288, 91)],
        &[(192, 240, 72), (240, 288, 73)],
    ]);
    for (i, track) in score.tracks.iter_mut().enumerate() {
        for note in &mut track.parts[0] {
            note.vel = if i == 1 || i == 2 { 15 } else { 8 };
        }
    }
    let selected = anchors(&score);
    assert!(selected.contains(&(48, 84)), "{selected:?}");
    assert!(selected.contains(&(240, 91)), "{selected:?}");
}
