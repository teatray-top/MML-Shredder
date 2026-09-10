//! 악보 비교의 음표 집계와 동시발음 계산을 검사합니다.

use std::collections::BTreeMap;

use mmlfold::core::track_from_mml;
use mmlfold::verify::{max_polyphony, verify_scores};
use mmlfold::{Note, Score};

/// 테스트용 음표를 만듭니다.
fn note(on: i64, off: i64, pitch: i32) -> Note {
    Note {
        on,
        off,
        pitch,
        vel: 8,
        src: (0, 0),
    }
}

#[test]
/// 빈 입력·동시 경계·0길이 음의 동시발음 집계를 검사합니다.
fn polyphony_handles_empty_simultaneous_edges_and_zero_duration() {
    assert_eq!(max_polyphony(&[]), 0);
    assert_eq!(max_polyphony(&[note(0, 12, 60), note(12, 24, 60)]), 1);
    assert_eq!(
        max_polyphony(&[note(0, 24, 60), note(6, 18, 64), note(12, 12, 67)]),
        2
    );
}

#[test]
/// 중복 음표가 개수를 포함해 비교되는지 검사합니다.
fn duplicate_notes_are_counted_as_a_multiset() {
    let source = Score {
        tracks: vec![mmlfold::Track {
            parts: vec![vec![note(0, 12, 60)]],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut output = source.clone();
    output.tracks[0].parts[0].push(note(0, 12, 60));
    let report = verify_scores(&source, &output);
    assert!(report.contains("exactly-preserved 1 (50.00% of output)"));
    assert!(report.contains("output notes not present verbatim in input: 1"));
    assert!(report.contains("max polyphony: 2"));
}

#[test]
/// 비교 보고서의 템포 차이와 새 문법 감지를 검사합니다.
fn fidelity_reports_tempo_and_novel_dialect() {
    let make = |text: &str| Score {
        tracks: vec![track_from_mml(format!("MML@{text};"), BTreeMap::new(), 0).unwrap()],
        ..Default::default()
    };
    let source = make("t120c4");
    assert!(verify_scores(&source, &source).contains("exactly-preserved 1 (100.00% of output)"));
    assert!(
        verify_scores(&source, &source).contains("constructs absent from the source dialect: none")
    );
    let report = verify_scores(&source, &make("t150c4.."));
    assert!(report.contains("tempo events exact: false"));
    assert!(report.contains("c#.."));
}

#[test]
/// 빈 악보 비교에 무한값이 없는지 검사합니다.
fn empty_verification_is_finite() {
    let report = verify_scores(&Score::default(), &Score::default());
    assert!(report.contains("max polyphony: 0"));
    assert!(report.contains("0.00% of output"));
}
