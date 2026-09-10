//! 파일 저장의 입력 보호와 편곡·분할 출력 구성을 검사합니다.

use mmlfold::{core, fold::FoldOptions, split::SplitOptions, workflow};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// 이름이 겹치지 않는 임시 경로를 만듭니다.
fn temp() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mmlfold-export-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    path
}

/// 악보의 각 파트를 MML 문자열로 인코딩합니다.
fn mmi_parts(score: &mmlfold::Score) -> Vec<Vec<String>> {
    score
        .tracks
        .iter()
        .map(|track| {
            track
                .mml
                .strip_prefix("MML@")
                .unwrap()
                .strip_suffix(';')
                .unwrap()
                .split(',')
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// 출력 파일이 MMI만 포함하는지 검사합니다.
fn assert_mmi_only(result: &workflow::WorkResult) {
    // Diagnostics remain available internally, but are never export artifacts.
    assert!(!result.report.is_empty());
    assert!(!result.artifacts.is_empty());
    assert!(
        result
            .artifacts
            .iter()
            .all(|artifact| artifact.name.ends_with(".mmi"))
    );
}

#[test]
/// 저장 전 입력 보호와 경로 충돌 감지를 검사합니다.
fn input_is_protected_and_conflicts_are_detected_before_writing() {
    let dir = temp();
    let source = dir.join("source.mmi");
    fs::write(&source, "original").unwrap();
    let fresh = dir.join("fresh.mmi");
    assert!(
        workflow::write_files(
            &[
                (fresh.clone(), "new".into()),
                (source.clone(), "changed".into())
            ],
            false,
            &[]
        )
        .is_err()
    );
    assert!(!fresh.exists());
    assert!(
        workflow::write_files(
            &[(source.clone(), "changed".into())],
            true,
            std::slice::from_ref(&source)
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "original");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
/// 새 폴더 저장과 트랙별 파일의 재결합을 검사합니다.
fn gui_exports_into_new_folders_and_track_files_recombine_exactly() {
    let score = core::parse_mmi("[mml-score]\nversion=1\ntempo=0T120\nmml-track=MML@t120cdef,g1,c1;\nname=Test\nvisible=true\n[time-signature]\n0=4/4\n").unwrap();
    let result = workflow::arrange(&score, &FoldOptions::default(), "music", true).unwrap();
    assert_mmi_only(&result);
    let dir = temp();
    let first = workflow::export_new_folder(&result, &dir, "music").unwrap();
    let second = workflow::export_new_folder(&result, &dir, "music").unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read_dir(&first).unwrap().count(), 3);
    assert!(!first.join("music_folded_game.txt").exists());
    let full = core::load_mmi(&first.join("music_folded.mmi")).unwrap();
    let mut single_notes = Vec::new();
    for i in 1..=2 {
        let track = core::load_mmi(&first.join(format!("music_folded_t{i}.mmi"))).unwrap();
        assert_eq!(track.tempos(), full.tempos());
        single_notes.extend(track.all_notes());
    }
    let key = |n: &mmlfold::Note| (n.on, n.off, n.pitch, n.vel);
    let mut a: Vec<_> = full.all_notes().iter().map(key).collect();
    let mut b: Vec<_> = single_notes.iter().map(key).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(windows)]
/// Windows 대소문자 경로 별칭으로 원본을 덮지 않는지 검사합니다.
fn windows_case_aliases_cannot_replace_a_score_with_its_report() {
    let dir = temp();
    let upper = dir.join("Result.mmi");
    let lower = dir.join("result.mmi");
    assert!(
        workflow::write_files(
            &[(upper.clone(), "score".into()), (lower, "report".into())],
            true,
            &[]
        )
        .is_err()
    );
    assert!(!upper.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
/// 검증 화면에 표시한 보고서를 그대로 저장하는지 검사합니다.
fn verification_exports_the_displayed_report() {
    let score = core::parse_mmi("[mml-score]\nmml-track=MML@c;\nvisible=true\n").unwrap();
    let compared = workflow::compare(&score, score.clone(), "test");
    assert!(compared.kind == workflow::WorkKind::Verify);
    assert_eq!(compared.artifacts.len(), 1);
    assert_eq!(compared.artifacts[0].text, compared.report);
    assert!(compared.report.contains("100.00%"));
}

#[test]
/// 분할 저장의 공통 시간축과 파트당 2400자 한도를 검사합니다.
fn scroll_exports_cut_all_tracks_on_one_timeline_with_2400_chars_per_part() {
    // 빠른 파트가 한도를 결정해도 느린 파트의 경계 지속음은 유지해야 합니다.
    let source = format!(
        "[mml-score]\nversion=1\ntempo=0T120\n\
         mml-track=MML@t120o4l16{},o3l8{},o2l4{};\n\
         name=First\npanpot=32\nvisible=true\n\
         mml-track=MML@t120o5l16{},o4l8{},o3l4{};\n\
         name=Second\npanpot=96\nvisible=true\n\
         [time-signature]\n0=4/4\n",
        "cdef".repeat(650),
        "ga".repeat(650),
        "ce".repeat(325),
        "fedc".repeat(650),
        "bd".repeat(650),
        "cg".repeat(325),
    );
    let score = core::parse_mmi(&source).unwrap();
    let result = workflow::scrolls(&score, &SplitOptions::default(), "ensemble").unwrap();
    assert_mmi_only(&result);
    let preview = result.split.as_ref().unwrap();
    assert_eq!(preview.limit, 2400);
    assert!(preview.chunks.len() > 1);
    assert_eq!(preview.chunks.first().unwrap().start, 0);
    assert!(
        (score.total_ticks()..=score.total_ticks() + 1)
            .contains(&preview.chunks.last().unwrap().end)
    );
    for adjacent in preview.chunks.windows(2) {
        assert_eq!(adjacent[0].end, adjacent[1].start);
    }
    // The displayed/playable score remains the entire ensemble.
    assert_eq!(result.score.all_notes(), score.all_notes());
    assert_eq!(
        core::serialize_mmi(&result.score),
        core::serialize_mmi(&score)
    );

    let dir = temp();
    let exported = workflow::export_new_folder(&result, &dir, "ensemble").unwrap();
    assert_eq!(
        fs::read_dir(&exported).unwrap().count(),
        preview.chunks.len()
    );
    for (index, chunk) in preview.chunks.iter().enumerate() {
        assert!(chunk.start < chunk.end);
        let score_file = exported.join(format!("ensemble_{:02}.mmi", index + 1));
        let saved = core::load_mmi(&score_file).unwrap();
        let artifact = result
            .artifacts
            .iter()
            .find(|artifact| artifact.name == format!("ensemble_{:02}.mmi", index + 1))
            .unwrap();
        assert_eq!(saved.tracks.len(), score.tracks.len());
        assert_eq!(chunk.part_widths.len(), saved.tracks.len());
        for (track_index, (track, original)) in saved.tracks.iter().zip(&score.tracks).enumerate() {
            // Python처럼 트랙 번호를 붙이고 누락된 재생 필드는 빈 값으로 저장합니다.
            assert_eq!(track.meta["name"], format!("Track{}", track_index + 1));
            for key in ["program", "songProgram", "panpot", "visible"] {
                assert_eq!(
                    track.meta.get(key).map(String::as_str),
                    Some(original.meta.get(key).map_or("", String::as_str))
                );
            }
            assert_eq!(track.parts.len(), original.parts.len());
            let part_texts: Vec<_> = track
                .mml
                .strip_prefix("MML@")
                .unwrap()
                .strip_suffix(';')
                .unwrap()
                .split(',')
                .collect();
            assert_eq!(artifact.game_parts[track_index], part_texts);
            assert!(part_texts.iter().all(|text| text.chars().count() <= 2400));
            let widths: Vec<_> = artifact.game_parts[track_index]
                .iter()
                .map(|text| text.chars().count())
                .collect();
            assert_eq!(chunk.part_widths[track_index], widths);
            assert!(widths.iter().all(|width| *width <= 2400));

            for (part, original_part) in track.parts.iter().zip(&original.parts) {
                let expected: Vec<_> = original_part
                    .iter()
                    .filter(|note| note.on < chunk.end && note.off > chunk.start)
                    .map(|note| mmlfold::Note {
                        on: note.on.max(chunk.start) - chunk.start,
                        off: note.off.min(chunk.end) - chunk.start,
                        ..note.clone()
                    })
                    // 기본 출력의 짧은 조각 생략은 Python에 맞추고 엄격 분할은 따로 검사합니다.
                    .filter(|note| note.off - note.on >= core::MIN_TICK)
                    .collect();
                assert_eq!(part, &expected);
            }
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
/// MMI 분할의 템포·지속음 보존과 출력 형식을 검사합니다.
fn scroll_mmi_preserves_tempo_commands_and_held_notes_without_direct_mml_export() {
    let score = core::parse_mmi(
        "[mml-score]\nversion=1\ntempo=0T120,48T180\n\
         mml-track=MML@t120c8t180&c8,r8g8,e4;\nname=Test\nvisible=true\n",
    )
    .unwrap();
    let result = workflow::scrolls(&score, &SplitOptions::default(), "tempo").unwrap();
    assert_mmi_only(&result);
    let artifact = result
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "tempo_01.mmi")
        .unwrap();
    let editable = core::parse_mmi(&artifact.text).unwrap();
    assert_eq!(editable.all_notes(), score.all_notes());
    assert_eq!(editable.tempos(), score.tempos());
    assert_eq!(artifact.tempo_note_splits, 0);
    assert_eq!(artifact.game_parts, mmi_parts(&editable));
    assert!(artifact.game_parts[0][0].contains("t180&"));

    assert!(
        !result
            .artifacts
            .iter()
            .any(|artifact| artifact.name == "tempo_01.txt")
    );
    let widths: Vec<_> = artifact.game_parts[0]
        .iter()
        .map(|part| part.chars().count())
        .collect();
    assert_eq!(result.split.unwrap().chunks[0].part_widths[0], widths);
}

#[test]
/// 요청한 MMI 트랙 파일만 저장하는지 검사합니다.
fn folding_exports_only_requested_mmi_track_files_without_sidecars() {
    let score = core::parse_mmi(
        "[mml-score]\ntempo=0T120,48T180\n\
         mml-track=MML@t120c8t180&c8,r8g8,e4;\nvisible=true\n",
    )
    .unwrap();
    let result = workflow::arrange(&score, &FoldOptions::default(), "tempo", true).unwrap();
    assert_mmi_only(&result);
    let game = workflow::game_export(&result.score).unwrap();
    let full = result
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "tempo_folded.mmi")
        .unwrap();
    assert_eq!(full.game_parts, game.parts);
    assert_eq!(
        full.game_parts,
        mmi_parts(&core::parse_mmi(&full.text).unwrap())
    );
    assert_eq!(full.tempo_note_splits, 0);
    assert_eq!(full.text, core::serialize_mmi(&result.score));
    assert!(
        !result
            .artifacts
            .iter()
            .any(|artifact| artifact.name == "tempo_folded_game.txt")
    );
    for index in 0..game.parts.len() {
        let single = result
            .artifacts
            .iter()
            .find(|artifact| artifact.name == format!("tempo_folded_t{}.mmi", index + 1))
            .unwrap();
        let single_score = core::parse_mmi(&single.text).unwrap();
        assert_eq!(single.game_parts, mmi_parts(&single_score));
        assert_eq!(single.tempo_note_splits, 0);
    }
    let combined_only = workflow::arrange(&score, &FoldOptions::default(), "tempo", false).unwrap();
    assert_mmi_only(&combined_only);
    assert_eq!(combined_only.artifacts.len(), 1);
    assert_eq!(combined_only.artifacts[0].name, "tempo_folded.mmi");
    assert_eq!(combined_only.artifacts[0].text, full.text);
}

#[test]
/// 분할 MMI에서 템포를 가로지르는 음을 변형하지 않는지 검사합니다.
fn scroll_mmi_does_not_mute_or_shorten_notes_crossing_tempo_changes() {
    let score = core::parse_mmi(
        "[mml-score]\ntempo=0T120,48T180\n\
         mml-track=MML@t120c8t180&c8,e4,g4;\nvisible=true\n",
    )
    .unwrap();
    let result = workflow::scrolls(&score, &SplitOptions::default(), "held").unwrap();
    assert_mmi_only(&result);
    let artifact = result
        .artifacts
        .iter()
        .find(|artifact| artifact.name == "held_01.mmi")
        .unwrap();
    assert_eq!(artifact.tempo_note_splits, 0);
    let saved = core::parse_mmi(&artifact.text).unwrap();
    assert_eq!(saved.all_notes(), score.all_notes());
    assert_eq!(artifact.game_parts, mmi_parts(&saved));
    for (part_index, text) in artifact.game_parts[0].iter().enumerate() {
        assert!(!text.contains("v0"));
        let (notes, _, _) = core::parse_part(text, (0, part_index)).unwrap();
        assert_eq!(notes, score.tracks[0].parts[part_index]);
    }
}

#[test]
/// 내부 파트 정보가 원문 명령과 빈 슬롯을 유지하는지 검사합니다.
fn internal_part_metadata_keeps_literal_commands_and_empty_slots_across_tracks() {
    let score = core::parse_mmi(
        "[mml-score]\ntempo=0T120,48T180\n\
         mml-track=MML@t120c8t180&c8,,v0r4;\nvisible=true\n\
         mml-track=MML@t120e8t180&e8,g4,;\nvisible=true\n",
    )
    .unwrap();
    let exported = workflow::game_export(&score).unwrap();
    assert_eq!(exported.tempo_note_splits, 0);
    assert_eq!(exported.parts, mmi_parts(&score));
    let combined = exported
        .parts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(combined, "t120c8t180&c8,,v0r4,t120e8t180&e8,g4,");
}
