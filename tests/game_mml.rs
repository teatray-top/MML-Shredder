//! 게임용 MML 인코딩의 템포 배치와 음표 보존을 검사합니다.

use mmlfold::{Note, Tempo, Tick, core, game_mml};

type PlayedNote = (Tick, Tick, i32, i32);

#[derive(Default)]
struct Parsed {
    notes: Vec<PlayedNote>,
    tempos: Vec<Tempo>,
}

// 이 해석기는 실험 인코더 검사 전용이며 기본 출력의 정답은 Python 파일입니다.
/// 게임용 MML에서 실제 음표와 템포를 해석합니다.
fn parse_game_part(text: &str) -> Parsed {
    /// MML의 현재 위치에서 정수를 읽습니다.
    fn number(bytes: &[u8], cursor: &mut usize) -> Option<i64> {
        let start = *cursor;
        let mut value = 0;
        while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() {
            value = value * 10 + i64::from(bytes[*cursor] - b'0');
            *cursor += 1;
        }
        (*cursor != start).then_some(value)
    }
    /// 점 하나를 소비하고 겹점 출력은 오류로 처리합니다.
    fn dotted(bytes: &[u8], cursor: &mut usize) -> bool {
        if bytes.get(*cursor) == Some(&b'.') {
            *cursor += 1;
            assert_ne!(bytes.get(*cursor), Some(&b'.'), "multiple-dot output");
            true
        } else {
            false
        }
    }

    let mut result = Parsed::default();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut time = 0;
    let mut octave = 4;
    let mut volume = 8;
    let mut length = 4;
    let mut default_dot = false;
    let mut tied = false;
    let mut tempo_since_note = false;
    while cursor < bytes.len() {
        let command = bytes[cursor];
        cursor += 1;
        match command {
            b'o' => octave = number(bytes, &mut cursor).unwrap() as i32,
            b'v' => volume = number(bytes, &mut cursor).unwrap() as i32,
            b'l' => {
                length = number(bytes, &mut cursor).unwrap();
                default_dot = dotted(bytes, &mut cursor);
            }
            b't' => {
                result
                    .tempos
                    .push((time, number(bytes, &mut cursor).unwrap() as i32));
                tempo_since_note = true;
            }
            b'>' => octave += 1,
            b'<' => octave -= 1,
            b'&' => tied = true,
            b'r' | b'a'..=b'g' => {
                let mut accidental = 0;
                if let Some(sign) = bytes.get(cursor) {
                    if *sign == b'+' || *sign == b'#' {
                        accidental = 1;
                        cursor += 1;
                    } else if *sign == b'-' {
                        accidental = -1;
                        cursor += 1;
                    }
                }
                let explicit_length = number(bytes, &mut cursor);
                let explicit_dot = dotted(bytes, &mut cursor);
                let dot = explicit_dot || (explicit_length.is_none() && default_dot);
                let base = 384 / explicit_length.unwrap_or(length);
                let duration = base + if dot { base / 2 } else { 0 };
                if command != b'r' {
                    let step = match command {
                        b'c' => 0,
                        b'd' => 2,
                        b'e' => 4,
                        b'f' => 5,
                        b'g' => 7,
                        b'a' => 9,
                        b'b' => 11,
                        _ => unreachable!(),
                    };
                    let pitch = (octave + 1) * 12 + step + accidental;
                    if tied {
                        assert!(
                            !tempo_since_note,
                            "game-unsupported tie across tempo: {text}"
                        );
                        let previous = result.notes.last_mut().unwrap();
                        assert_eq!(previous.2, pitch);
                        assert_eq!(previous.1, time);
                        previous.1 += duration;
                    } else {
                        result.notes.push((time, time + duration, pitch, volume));
                    }
                } else {
                    assert!(!tied, "tied rest");
                }
                time += duration;
                tied = false;
                tempo_since_note = false;
            }
            _ => panic!("unexpected emitted command: {}", char::from(command)),
        }
    }
    result
}

/// 테스트용 음표를 만듭니다.
fn note(on: Tick, off: Tick, pitch: i32) -> Note {
    Note {
        on,
        off,
        pitch,
        vel: 8,
        src: (0, 0),
    }
}

/// 소리 나는 음표만 비교용 목록으로 모읍니다.
fn audible(notes: &[Note]) -> Vec<PlayedNote> {
    notes
        .iter()
        .filter(|note| note.vel > 0)
        .map(|note| (note.on, note.off, note.pitch, note.vel))
        .collect()
}

/// MML을 해석해 소리 나는 음표를 모읍니다.
fn parsed_audible(parsed: &Parsed) -> Vec<PlayedNote> {
    parsed
        .notes
        .iter()
        .copied()
        .filter(|note| note.3 > 0)
        .collect()
}

/// 전체 트랙의 템포 이벤트를 모읍니다.
fn all_tempos(parts: &[String]) -> Vec<Tempo> {
    let mut tempos = parts
        .iter()
        .flat_map(|part| parse_game_part(part).tempos)
        .collect::<Vec<_>>();
    tempos.sort();
    tempos
}

#[test]
/// 다른 파트의 음표 경계에 템포를 배치해 음을 보존하는지 검사합니다.
fn distributes_tempo_at_another_parts_note_boundary_without_changing_notes() {
    let parts = vec![
        vec![note(0, 192, 60)],
        vec![note(0, 96, 64), note(96, 192, 67)],
        vec![],
    ];
    let tempos = vec![(0, 120), (96, 150)];
    let game = game_mml::emit_track(&parts, &tempos, 192).unwrap();
    assert_eq!(game.parts.len(), parts.len());
    assert_eq!(game.tempo_note_splits, 0);
    assert_eq!(parse_game_part(&game.parts[1]).tempos, vec![(96, 150)]);
    assert_eq!(all_tempos(&game.parts), tempos);
    for (part, emitted) in parts.iter().zip(&game.parts) {
        assert_eq!(audible(part), parsed_audible(&parse_game_part(emitted)));
    }
}

#[test]
/// 무음 음표를 넣지 않고 정확한 쉼표에 템포를 배치하는지 검사합니다.
fn uses_an_exact_rest_gap_without_inserting_legacy_silent_notes() {
    let parts = vec![
        vec![note(0, 192, 60)],
        vec![note(0, 48, 64), note(144, 192, 67)],
    ];
    let game = game_mml::emit_track(&parts, &[(0, 120), (96, 180)], 192).unwrap();
    assert_eq!(game.tempo_note_splits, 0);
    let chord = parse_game_part(&game.parts[1]);
    assert_eq!(chord.tempos, vec![(96, 180)]);
    assert_eq!(chord.notes, audible(&parts[1]));
    assert_eq!(parse_game_part(&game.parts[0]).notes, audible(&parts[0]));
}

#[test]
/// 파트 선택에서 표현 불가능한 쉼표 조각을 거르는지 검사합니다.
fn rejects_unencodable_rest_fragments_when_selecting_a_part() {
    let parts = vec![vec![note(100, 192, 60)], vec![note(0, 192, 64)], vec![]];
    let game = game_mml::emit_track(&parts, &[(0, 120), (96, 180)], 192).unwrap();
    assert_eq!(game.tempo_note_splits, 0);
    assert_eq!(parse_game_part(&game.parts[2]).tempos, vec![(96, 180)]);
    assert_eq!(parse_game_part(&game.parts[0]).notes, audible(&parts[0]));
}

#[test]
/// 모든 음이 지속 중일 때 최소 꼬리를 줄이고 음량을 복원하는지 검사합니다.
fn all_held_fallback_shortens_the_least_remaining_note_and_restores_volume() {
    let mut next_note = note(144, 240, 69);
    next_note.vel = 11;
    let parts = vec![
        vec![note(0, 192, 60)],
        vec![note(0, 144, 64), next_note],
        vec![note(0, 168, 67)],
    ];
    let game = game_mml::emit_track(&parts, &[(0, 120), (96, 180)], 240).unwrap();
    assert_eq!(game.tempo_note_splits, 1);
    let chord = parse_game_part(&game.parts[1]);
    assert_eq!(chord.tempos, vec![(96, 180)]);
    assert_eq!(
        chord.notes,
        vec![(0, 96, 64, 8), (96, 144, 64, 0), (144, 240, 69, 11)]
    );
    assert_eq!(parse_game_part(&game.parts[0]).notes, audible(&parts[0]));
    assert_eq!(parse_game_part(&game.parts[2]).notes, audible(&parts[2]));
}

#[test]
/// 중복 템포가 들리는 음을 자르지 않는지 검사합니다.
fn duplicate_and_redundant_tempos_do_not_create_audible_truncations() {
    let parts = vec![vec![note(0, 192, 60)]];
    let game =
        game_mml::emit_track(&parts, &[(0, 180), (0, 120), (96, 150), (96, 120)], 192).unwrap();
    assert_eq!(game.tempo_note_splits, 0);
    assert_eq!(all_tempos(&game.parts), vec![(0, 120)]);
    assert_eq!(parse_game_part(&game.parts[0]).notes, audible(&parts[0]));
}

#[test]
/// 첫 템포가 늦게 나올 때 초기 기본 템포를 넣는지 검사합니다.
fn restores_default_initial_tempo_before_a_later_first_tempo() {
    let parts = vec![vec![note(0, 96, 60), note(96, 192, 64)]];
    let game = game_mml::emit_track(&parts, &[(96, 180)], 192).unwrap();
    assert_eq!(all_tempos(&game.parts), vec![(0, 120), (96, 180)]);
    assert_eq!(game.tempo_note_splits, 0);
}

#[test]
/// 템포 없는 악보가 기본 템포로 시작하는지 검사합니다.
fn score_without_tempo_commands_explicitly_starts_at_default_tempo() {
    let parts = vec![vec![note(0, 96, 60)]];
    let game = game_mml::emit_track(&parts, &[], 96).unwrap();
    assert_eq!(all_tempos(&game.parts), vec![(0, 120)]);
    assert_eq!(parse_game_part(&game.parts[0]).notes, audible(&parts[0]));
}

#[test]
/// 노래 파트에도 독립 템포 시퀀스를 넣는지 검사합니다.
fn song_part_receives_its_own_tempo_sequence() {
    let parts = vec![
        vec![note(0, 96, 60), note(96, 192, 64)],
        vec![],
        vec![],
        vec![note(0, 192, 72)],
    ];
    let tempos = vec![(0, 120), (96, 180)];
    let game = game_mml::emit_track(&parts, &tempos, 192).unwrap();
    assert_eq!(game.parts.len(), 4);
    assert_eq!(game.tempo_note_splits, 1);
    assert_eq!(all_tempos(&game.parts[..3]), tempos);
    assert_eq!(parse_game_part(&game.parts[3]).tempos, tempos);
    assert_eq!(
        parse_game_part(&game.parts[3]).notes,
        vec![(0, 96, 72, 8), (96, 192, 72, 0)]
    );
}

#[test]
/// 같은 무음 연장부의 여러 템포를 한 번의 절단으로 세는지 검사합니다.
fn multiple_tempos_in_the_same_muted_continuation_count_one_shortened_note() {
    let parts = vec![vec![note(0, 384, 60)]];
    let game = game_mml::emit_track(&parts, &[(0, 120), (96, 180), (192, 90)], 384).unwrap();
    assert_eq!(game.tempo_note_splits, 1);
    assert_eq!(
        parsed_audible(&parse_game_part(&game.parts[0])),
        vec![(0, 96, 60, 8)]
    );
}

#[test]
/// 표현 불가능한 템포 분할을 반올림하지 않는지 검사합니다.
fn refuses_to_round_an_unrepresentable_tempo_split() {
    let parts = vec![vec![note(0, 192, 60)]];
    let error = game_mml::emit_track(&parts, &[(0, 120), (3, 180)], 192).unwrap_err();
    assert!(error.to_string().contains("without rounding"));
    assert_eq!(parts[0][0].off, 192);
}

#[test]
/// 실곡 게임 출력의 공격 보존과 불가피한 절단 보고를 검사합니다.
fn fixture_game_output_preserves_every_onset_and_only_reports_unavoidable_gates() {
    let score = core::parse_mmi(include_str!("../Op39No11_full_6part.mmi")).unwrap();
    let mut gates_shortened = 0;
    for track in &score.tracks {
        let game = game_mml::emit_track(&track.parts, &track.tempos, score.total_ticks()).unwrap();
        let mut changed = 0;
        for (original, emitted) in track.parts.iter().zip(&game.parts) {
            let parsed = parse_game_part(emitted);
            let actual = parsed_audible(&parsed);
            let expected = audible(original);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(
                    (actual.0, actual.2, actual.3),
                    (expected.0, expected.2, expected.3)
                );
                assert!(actual.1 <= expected.1);
                changed += usize::from(actual.1 != expected.1);
            }
        }
        assert_eq!(changed, game.tempo_note_splits);
        gates_shortened += changed;
    }
    assert_eq!(gates_shortened, 3);
}

#[test]
/// 편곡·분할 내부 파트가 저장 MMI와 같고 2400자 이내인지 검사합니다.
fn folded_split_internal_parts_are_the_exact_mmi_parts_within_2400_chars() {
    let source = core::parse_mmi(include_str!("../Op39No11_full_6part.mmi")).unwrap();
    let folded =
        mmlfold::fold::fold_score(&source, &mmlfold::fold::FoldOptions::default()).unwrap();
    let split =
        mmlfold::split::split_score(&folded.score, &mmlfold::split::SplitOptions::default())
            .unwrap();
    for chunk in split.chunks {
        assert_eq!(chunk.tempo_note_splits, 0);
        assert_eq!(chunk.game_parts, chunk.mml_parts);
        for (track, game_parts) in chunk.score.tracks.iter().zip(&chunk.game_parts) {
            let stored_parts = track
                .mml
                .strip_prefix("MML@")
                .unwrap()
                .strip_suffix(';')
                .unwrap()
                .split(',')
                .collect::<Vec<_>>();
            assert_eq!(*game_parts, stored_parts);
            for (part_index, (notes, text)) in track.parts.iter().zip(game_parts).enumerate() {
                assert!(text.len() <= 2400);
                let (parsed, _, _) =
                    core::parse_part(text, notes.first().map_or((0, part_index), |note| note.src))
                        .unwrap();
                assert_eq!(
                    audible(&parsed),
                    audible(notes),
                    "chunk starts at {}",
                    chunk.start
                );
            }
        }
    }
}
