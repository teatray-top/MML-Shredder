//! MML 해석과 재출력에서 음표·템포·메타데이터 보존을 검사합니다.

use std::collections::BTreeMap;
use std::path::Path;

use mmlfold::Note;
use mmlfold::core::{
    emit_part, load_mmi, parse_mmi, parse_part, serialize_mmi, split_ticks, track_from_mml,
};

/// 테스트용 음표를 만듭니다.
fn note(on: i64, off: i64, pitch: i32, vel: i32) -> Note {
    Note {
        on,
        off,
        pitch,
        vel,
        src: (0, 0),
    }
}

#[test]
/// 기본 음가·점음표·절대 피치·붙임줄 해석을 검사합니다.
fn length_semantics_default_dots_absolute_pitch_and_ties() {
    let (notes, tempos, end) =
        parse_part("MML@t120o4l8.c&ct90&c16r16v11n48>f-32g+7..;", (2, 1)).unwrap();
    assert_eq!(tempos, vec![(0, 120), (144, 90)]);
    assert_eq!(notes.len(), 4);
    assert_eq!(
        notes[0],
        Note {
            on: 0,
            off: 168,
            pitch: 60,
            vel: 8,
            src: (2, 1)
        }
    );
    assert_eq!(
        (notes[1].on, notes[1].off, notes[1].pitch, notes[1].vel),
        (192, 264, 60, 11)
    );
    assert_eq!((notes[2].on, notes[2].off, notes[2].pitch), (264, 276, 76));
    // 길이 7은 54틱이며 겹점은 floor(54 × 1.75) = 94틱입니다.
    assert_eq!((notes[3].on, notes[3].off, notes[3].pitch), (276, 370, 80));
    assert_eq!(end, 370);
}

#[test]
/// 쉼표와 피치 변화가 붙임줄을 끊는지 검사합니다.
fn resting_breaks_a_tie_and_pitch_changes_create_new_attacks() {
    let (notes, _, end) = parse_part("c8&d8&r8&d8", (0, 0)).unwrap();
    assert_eq!(
        notes,
        vec![
            note(0, 48, 60, 8),
            note(48, 96, 62, 8),
            note(144, 192, 62, 8)
        ]
    );
    assert_eq!(end, 192);
}

#[test]
/// 정확한 틱 분해와 표현 불가능한 짧은 음가 오류를 검사합니다.
fn exact_tick_decomposition_and_short_duration_errors() {
    assert_eq!(split_ticks(0).unwrap(), Vec::<i64>::new());
    assert_eq!(split_ticks(700).unwrap().iter().sum::<i64>(), 700);
    for ticks in 6..=6000 {
        let atoms = split_ticks(ticks).unwrap();
        assert_eq!(atoms.iter().sum::<i64>(), ticks);
        assert!(atoms.iter().all(|&atom| atom >= 6));
    }
    for ticks in [-1, 1, 2, 3, 4, 5] {
        assert!(split_ticks(ticks).is_err(), "unexpectedly accepted {ticks}");
    }
}

#[test]
/// 지속음·쉼표 내부의 템포가 재출력 후에도 같은지 검사합니다.
fn tempos_inside_held_notes_and_rests_round_trip_exactly() {
    let notes = vec![note(48, 624, 65, 11), note(720, 816, 64, 9)];
    let tempos = vec![
        (0, 120),
        (24, 140),
        (96, 80),
        (192, 95),
        (192, 100),
        (624, 120),
        (672, 90),
        (864, 75),
    ];
    let encoded = emit_part(&notes, &tempos, 900).unwrap();
    let (decoded, decoded_tempos, end) = parse_part(&encoded, (0, 0)).unwrap();
    assert_eq!(decoded, notes, "encoded: {encoded}");
    assert_eq!(decoded_tempos, tempos, "encoded: {encoded}");
    assert_eq!(end, 864); // Source-compatible omission of final padding.
    assert!(encoded.contains('&'));
    assert!(!encoded.contains(".."));
}

#[test]
/// 이명동음 표기가 E·F의 옥타브를 바꾸지 않는지 검사합니다.
fn enharmonic_spelling_never_transposes_e_or_f() {
    // E#/Fb 대체 표기가 옥타브를 바꾸던 경계를 검사합니다.
    let notes = vec![
        note(0, 96, 53, 8),
        note(96, 192, 65, 8),
        note(192, 288, 76, 8),
        note(288, 384, 64, 8),
    ];
    let encoded = emit_part(&notes, &[], 384).unwrap();
    assert_eq!(
        parse_part(&encoded, (0, 0)).unwrap().0,
        notes,
        "encoded: {encoded}"
    );
    // 허용 옥타브 양끝의 B#/Cb도 포함합니다.
    let notes: Vec<Note> = (11..=120)
        .enumerate()
        .map(|(i, pitch)| note(i as i64 * 12, (i as i64 + 1) * 12, pitch, 8))
        .collect();
    let encoded = emit_part(&notes, &[], 1320).unwrap();
    assert_eq!(parse_part(&encoded, (0, 0)).unwrap().0, notes);
}

#[test]
/// 잘못된 명령과 표현 불가능한 구간의 오류를 검사합니다.
fn invalid_commands_and_unrepresentable_segments_have_errors() {
    for text in [
        "l",
        "t0c",
        "v999999999999999999999c",
        "o999999999999999999c",
        "c999",
        "n",
    ] {
        assert!(
            parse_part(text, (0, 0)).is_err(),
            "unexpectedly accepted {text}"
        );
    }
    assert!(emit_part(&[note(0, 100, 60, 8)], &[(2, 100)], 100).is_err());
    assert!(emit_part(&[note(2, 100, 60, 8)], &[], 100).is_err());
    assert!(emit_part(&[note(0, 96, 60, 8), note(48, 144, 62, 8)], &[], 144).is_err());
    assert!(emit_part(&[note(0, 96, 10, 8)], &[], 96).is_err());
}

#[test]
/// visible 표식 없는 트랙과 메타데이터 보존을 검사합니다.
fn container_preserves_metadata_and_tracks_without_visible_marker() {
    let raw = "\u{feff}[mml-score]\r\nversion=1\r\nmml-track=MML@c,d,e;\r\nname=First\r\ncustom=value=with=equals\r\nmml-track=MML@f;\r\nname=Last\r\n[time-signature]\r\n96=3/4\r\n";
    let score = parse_mmi(raw).unwrap();
    assert_eq!(score.tracks.len(), 2);
    assert_eq!(score.tracks[0].meta["custom"], "value=with=equals");
    assert_eq!(score.tracks[1].meta["name"], "Last");
    let decoded = parse_mmi(&serialize_mmi(&score)).unwrap();
    assert_eq!(decoded.all_notes(), score.all_notes());
    assert_eq!(decoded.tracks[0].meta, score.tracks[0].meta);
    assert_eq!(decoded.tail, score.tail);
    assert_eq!(decoded.head, score.head);
}

#[test]
/// 빈 파트 처리와 트랙 정규화를 검사합니다.
fn empty_parts_and_track_normalization_work() {
    let track = track_from_mml(",,".into(), BTreeMap::new(), 0).unwrap();
    assert_eq!(track.mml, "MML@,,;");
    assert_eq!(track.parts.len(), 3);
    assert!(track.parts.iter().all(Vec::is_empty));
    assert_eq!(emit_part(&[], &[], 999).unwrap(), "");
    assert_eq!(emit_part(&[], &[(0, 120)], 0).unwrap(), "t120");
}

#[test]
/// 여러 위치에 기록된 템포의 보존을 검사합니다.
fn distributed_tempos_survive_parts_tracks_and_score_metadata() {
    let raw = "[mml-score]\ntempo=0T100,192T80\nmml-track=MML@t120c1,r8t150c4,r4t90c4;\nvisible=true\nmml-track=MML@r4.t180c4;\nvisible=true\n";
    let score = parse_mmi(raw).unwrap();
    assert_eq!(score.tracks[0].tempos, vec![(0, 120), (48, 150), (96, 90)]);
    assert_eq!(
        score.tempos(),
        vec![(0, 100), (48, 150), (96, 90), (144, 180), (192, 80)]
    );
    assert_eq!(
        parse_mmi(&serialize_mmi(&score)).unwrap().tempos(),
        score.tempos()
    );
    let timeline = mmlfold::synth::Timeline::from_score(&score).unwrap();
    let expected = 0.5 * (60.0 / 100.0 + 60.0 / 150.0 + 60.0 / 90.0 + 60.0 / 180.0);
    assert!((timeline.tick_to_seconds(192.0) - expected).abs() < 1e-10);
}

#[test]
/// 로컬 실곡의 저장·재파싱에서 음표와 템포를 대조합니다.
fn provided_fixtures_round_trip_notes_and_tempos() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![
        root.join("Op39No11_full_6part.mmi"),
        root.join("Op39No11_solo_3part.mmi"),
        root.join("Op39No11_second_3part.mmi"),
    ];
    for directory in ["scrolls_ensemble", "scrolls_solo", "scrolls_second"] {
        paths.extend(
            std::fs::read_dir(root.join(directory))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "mmi")),
        );
    }
    let mut checked_notes = 0;
    for path in paths {
        let score = load_mmi(&path).unwrap();
        let reparsed = parse_mmi(&serialize_mmi(&score)).unwrap();
        assert_eq!(
            reparsed.all_notes(),
            score.all_notes(),
            "{}",
            path.display()
        );
        assert_eq!(reparsed.tempos(), score.tempos(), "{}", path.display());
        for (track_index, track) in score.tracks.iter().enumerate() {
            for (part_index, part) in track.parts.iter().enumerate() {
                let tempos = if part_index == 0 {
                    track.tempos.as_slice()
                } else {
                    &[]
                };
                let encoded =
                    emit_part(part, tempos, score.total_ticks()).unwrap_or_else(|error| {
                        panic!(
                            "{} track {track_index} part {part_index}: {error:#}",
                            path.display()
                        )
                    });
                let (notes, actual_tempos, _) =
                    parse_part(&encoded, (track_index, part_index)).unwrap();
                assert_eq!(
                    &notes,
                    part,
                    "{} track {track_index} part {part_index}",
                    path.display()
                );
                assert_eq!(
                    actual_tempos,
                    tempos,
                    "{} track {track_index} part {part_index}",
                    path.display()
                );
                checked_notes += notes.len();
            }
        }
    }
    assert!(
        checked_notes > 30_000,
        "expected substantial fixture coverage, got {checked_notes}"
    );
}
