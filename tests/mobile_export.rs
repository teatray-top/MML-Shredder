//! 모바일에서 표현할 수 없는 음가의 보정과 저장·미리보기 일치를 검사합니다.

use mmlfold::{
    Note, Score, core,
    fold::{FoldOptions, Gain, Layout},
    workflow,
};

/// 서로 다른 파트가 같은 경계를 공유하는 19틱 쉼표·115틱 음표를 만듭니다.
fn unsupported_score() -> Score {
    core::parse_mmi(
        "[mml-score]\nversion=1\ntempo=0T120\n\
         mml-track=MML@t120c4r20d20&d4,r20r4g6,e20&e4&e4;\nname=Test\n",
    )
    .unwrap()
}

/// 저장한 MMI와 미리보기의 음표·템포가 일치하는지 검사합니다.
fn assert_saved_preview(result: &workflow::WorkResult) {
    let saved = core::parse_mmi(&result.artifacts[0].text).unwrap();
    assert_eq!(saved.all_notes(), result.score.all_notes());
    assert_eq!(saved.tempos(), result.score.tempos());
}

#[test]
/// 공유 경계를 함께 보정하고 이미 표현 가능한 인접 경계는 보존합니다.
fn saving_repairs_mobile_lengths_without_rearranging_notes() {
    let original = unsupported_score();
    let unchanged = core::serialize_mmi(&original);
    let result = workflow::source_export(&original, "compatible").unwrap();
    assert_saved_preview(&result);
    assert_eq!(core::serialize_mmi(&original), unchanged);
    let parts = &result.score.tracks[0].parts;
    assert_eq!((parts[0][1].on, parts[0][1].off), (114, 230));
    assert_eq!((parts[1][0].on, parts[1][0].off), (114, 179));
    assert_eq!(parts[2][0].off, 210);
    for (old, new) in original.all_notes().iter().zip(result.score.all_notes()) {
        assert!((old.on - new.on).abs() <= 1);
        assert!((old.off - new.off).abs() <= 1);
        assert_eq!((old.pitch, old.vel, old.src), (new.pitch, new.vel, new.src));
    }
    let saved_again = workflow::source_export(&result.score, "compatible").unwrap();
    assert_eq!(saved_again.artifacts[0].text, result.artifacts[0].text);
}

#[test]
/// 편곡 결과와 트랙별 저장 파일이 같은 보정된 시간축을 사용합니다.
fn arrangement_and_individual_tracks_share_the_saved_timeline() {
    let options = FoldOptions {
        tracks: 2,
        parts: 3,
        layout: Layout::Voices,
        gain: Gain::None,
        ..FoldOptions::default()
    };
    let result = workflow::arrange(&unsupported_score(), &options, "ensemble", true).unwrap();
    assert_saved_preview(&result);
    for (track, artifact) in result.score.tracks.iter().zip(&result.artifacts[1..]) {
        let saved = core::parse_mmi(&artifact.text).unwrap();
        for (expected, actual) in track.parts.iter().zip(&saved.tracks[0].parts) {
            let timing = |notes: &[Note]| {
                notes
                    .iter()
                    .map(|n| (n.on, n.off, n.pitch, n.vel))
                    .collect::<Vec<_>>()
            };
            assert_eq!(timing(expected), timing(actual));
        }
    }
}

#[test]
/// 많은 구간을 보정해도 시간 오차가 누적되지 않습니다.
fn repeated_repairs_do_not_accumulate_clock_drift() {
    let text = format!("[mml-score]\nmml-track=MML@t120{};\n", "c20r20".repeat(500));
    let original = core::parse_mmi(&text).unwrap();
    let result = workflow::source_export(&original, "long").unwrap();
    assert_eq!(original.all_notes().len(), result.score.all_notes().len());
    for (old, new) in original.all_notes().iter().zip(result.score.all_notes()) {
        assert!((old.on - new.on).abs() <= 1);
        assert!((old.off - new.off).abs() <= 1);
    }
    assert_saved_preview(&result);
}

#[test]
/// 이미 모바일 호환인 셋잇단음표와 원문 MML을 그대로 저장합니다.
fn supported_triplets_keep_the_original_bytes() {
    let original =
        core::parse_mmi("[mml-score]\ntempo=0T120\nmml-track=MML@t120l12cdefgab>c,r6c6,c4.;\n")
            .unwrap();
    let result = workflow::source_export(&original, "triplets").unwrap();
    assert_eq!(result.artifacts[0].text, core::serialize_mmi(&original));
    assert_saved_preview(&result);
}
