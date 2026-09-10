//! 성부 배치와 음표 선택, 꼬리 절단의 불변 조건을 검사합니다.

use std::collections::BTreeMap;

use mmlfold::core::{load_mmi, parse_mmi, serialize_mmi};
use mmlfold::fold::{FoldOptions, Gain, Layout, fold_score};
use mmlfold::{Note, Score, Track};

/// 음표나 MML 문자열로 테스트 악보를 만듭니다.
fn score(voices: &[&[(i64, i64, i32, i32)]]) -> Score {
    Score {
        head: vec!["[mml-score]".into(), "title=Fold test".into()],
        tracks: voices
            .iter()
            .enumerate()
            .map(|(track, voice)| Track {
                parts: vec![
                    voice
                        .iter()
                        .map(|&(on, off, pitch, vel)| Note {
                            on,
                            off,
                            pitch,
                            vel,
                            src: (track, 0),
                        })
                        .collect(),
                ],
                meta: BTreeMap::from([
                    ("name".into(), format!("Voice{track}")),
                    ("program".into(), "0".into()),
                    ("visible".into(), "true".into()),
                ]),
                ..Track::default()
            })
            .collect(),
        tail: vec!["[time-signature]".into(), "0=4/4".into()],
    }
}

/// 악보를 비교용 음표 목록으로 펼칩니다.
fn events(score: &Score) -> Vec<(i64, i64, i32, i32)> {
    let mut notes: Vec<_> = score
        .all_notes()
        .into_iter()
        .map(|n| (n.on, n.off, n.pitch, n.vel))
        .collect();
    notes.sort_unstable();
    notes
}

/// 시작·피치·음량별로 음의 끝을 모읍니다.
fn attacks_with_tails(score: &Score) -> BTreeMap<(i64, i32, i32), Vec<i64>> {
    let mut attacks = BTreeMap::<_, Vec<_>>::new();
    for note in score.all_notes() {
        attacks
            .entry((note.on, note.pitch, note.vel))
            .or_default()
            .push(note.off);
    }
    for tails in attacks.values_mut() {
        tails.sort_unstable();
    }
    attacks
}

/// 모든 공격을 유지하고 꼬리만 줄였는지 검사합니다.
fn assert_same_attacks_with_bounded_tails(output: &Score, baseline: &Score) {
    let expected = attacks_with_tails(baseline);
    let actual = attacks_with_tails(output);
    assert_eq!(
        actual.len(),
        expected.len(),
        "No attack may be added or lost"
    );
    for (attack @ (on, _, _), original_tails) in expected {
        let tails = actual.get(&attack).expect("Every original attack remains");
        assert_eq!(
            tails.len(),
            original_tails.len(),
            "Attack multiplicity changed: {attack:?}"
        );
        for (&tail, original_tail) in tails.iter().zip(original_tails) {
            assert!(
                tail >= on + 6,
                "Attack {attack:?} was shortened below six ticks"
            );
            assert!(tail <= original_tail, "Attack {attack:?} was lengthened");
        }
    }
}

/// 출력이 원본 공격의 부분집합이며 꼬리가 늘지 않았는지 검사합니다.
fn assert_attack_subset_with_bounded_tails(output: &Score, source: &Score) {
    let originals = attacks_with_tails(source);
    for (attack @ (on, _, _), tails) in attacks_with_tails(output) {
        let source_tails = originals.get(&attack).expect("No new attack may be added");
        assert!(
            tails.len() <= source_tails.len(),
            "Attack multiplicity grew: {attack:?}"
        );
        // 긴 꼬리부터 대응시켜 중복 공격 생략을 길이 연장으로 오판하지 않습니다.
        for (&tail, &source_tail) in tails.iter().rev().zip(source_tails.iter().rev()) {
            assert!(
                tail >= on + 6,
                "Attack {attack:?} was shortened below six ticks"
            );
            assert!(tail <= source_tail, "Attack {attack:?} was lengthened");
        }
    }
}

/// 음표 공격과 출력 트랙·파트의 대응을 구합니다.
fn attack_placements(score: &Score) -> Vec<(i64, i64, i32, usize, usize)> {
    let mut placements: Vec<_> = score
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, track)| {
            track.parts.iter().enumerate().flat_map(move |(pi, part)| {
                part.iter().map(move |n| (n.on, n.off, n.pitch, ti, pi))
            })
        })
        .collect();
    placements.sort_unstable();
    placements
}

/// 각 파트에서 음이 겹치지 않는지 검사합니다.
fn assert_monophonic(score: &Score) {
    for track in &score.tracks {
        for part in &track.parts {
            assert!(part.windows(2).all(|w| w[0].off <= w[1].on));
        }
    }
}

/// 지정한 음표가 배치된 트랙·파트를 찾습니다.
fn destination(output: &Score, original: &Note) -> (usize, usize) {
    let matches: Vec<_> = output
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, track)| {
            track
                .parts
                .iter()
                .enumerate()
                .filter_map(move |(pi, part)| {
                    part.iter()
                        .any(|n| {
                            (n.on, n.off, n.pitch, n.vel)
                                == (original.on, original.off, original.pitch, original.vel)
                        })
                        .then_some((ti, pi))
                })
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "test note must have one exact destination"
    );
    matches[0]
}

/// 지정한 배치 방식으로 두 파트에 편곡합니다.
fn fold_two_parts(source: &Score, layout: Layout) -> Score {
    let result = fold_score(
        source,
        &FoldOptions {
            tracks: 1,
            parts: 2,
            layout,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert_eq!(events(&result.score), events(source));
    assert_eq!(result.stats.dropped, 0);
    assert_eq!(result.stats.truncated, 0);
    assert_eq!(result.score.tracks.len(), 1);
    assert_eq!(result.score.tracks[0].parts.len(), 2);
    assert_monophonic(&result.score);
    result.score
}

#[test]
/// 성부 진입과 음역 교차에서 파트 연속성을 검사합니다.
fn source_layouts_keep_continuing_parts_through_entries_and_pitch_crossings() {
    for source in [
        score(&[
            &[(0, 24, 72, 8), (24, 48, 60, 8), (48, 72, 74, 8)],
            &[(24, 48, 84, 8)],
        ]),
        score(&[
            &[(0, 24, 84, 8), (24, 48, 48, 8)],
            &[(0, 24, 48, 8), (24, 48, 84, 8)],
        ]),
    ] {
        for layout in [Layout::Voices, Layout::Hands] {
            let output = fold_two_parts(&source, layout);
            for track in &source.tracks {
                let phrase = &track.parts[0];
                let first = destination(&output, &phrase[0]);
                assert!(phrase.iter().all(|n| destination(&output, n) == first));
            }
        }
    }
}

#[test]
/// 짧은 선율 쉼표를 긴 반주가 가로막지 않는지 검사합니다.
fn short_phrase_rest_is_not_filled_by_a_blocking_accompaniment() {
    let source = score(&[&[(0, 24, 72, 8), (48, 72, 74, 8)], &[(24, 120, 67, 8)]]);
    let output = fold_two_parts(&source, Layout::Hands);
    let phrase = &source.tracks[0].parts[0];
    let melody_part = destination(&output, &phrase[0]);
    assert_eq!(destination(&output, &phrase[1]), melody_part);
    assert_ne!(
        destination(&output, &source.tracks[1].parts[0][0]),
        melody_part
    );
}

#[test]
/// 불가피한 파트 이동 뒤 불필요하게 복귀하지 않는지 검사합니다.
fn unavoidable_move_does_not_bounce_back_when_the_old_part_becomes_free() {
    let source = score(&[
        &[
            (0, 24, 72, 8),
            (48, 72, 74, 8),
            (72, 96, 76, 8),
            (96, 120, 77, 8),
        ],
        &[(24, 84, 84, 8)],
        &[(0, 48, 48, 8)],
    ]);
    for layout in [Layout::Voices, Layout::Hands] {
        let output = fold_two_parts(&source, layout);
        let phrase = &source.tracks[0].parts[0];
        let moved_part = destination(&output, &phrase[1]);
        assert_ne!(destination(&output, &phrase[0]), moved_part);
        assert!(
            phrase[1..]
                .iter()
                .all(|n| destination(&output, n) == moved_part)
        );
    }
}

#[test]
/// 긴 쉼표 뒤 새 구절의 배치 방식 적용을 검사합니다.
fn a_long_rest_starts_a_new_phrase_with_the_selected_layout() {
    let source = score(&[&[(0, 24, 72, 8), (192, 216, 50, 8)], &[(192, 216, 84, 8)]]);
    let output = fold_two_parts(&source, Layout::Roles);
    let phrase = &source.tracks[0].parts[0];
    assert_ne!(
        destination(&output, &phrase[0]),
        destination(&output, &phrase[1])
    );
}

#[test]
/// 원본 성부가 바뀌어도 주선율 역할을 유지하는지 검사합니다.
fn roles_follow_the_primary_melody_when_it_changes_original_source() {
    let source = score(&[
        &[(0, 24, 84, 8), (24, 48, 48, 8)],
        &[(0, 24, 48, 8), (24, 48, 84, 8)],
    ]);
    let output = fold_two_parts(&source, Layout::Roles);
    // 주선율은 두 원본 성부를 오가므로 고정 파트보다 역할이 우선입니다.
    assert_eq!(destination(&output, &source.tracks[0].parts[0][0]), (0, 0));
    assert_eq!(destination(&output, &source.tracks[1].parts[0][1]), (0, 0));
    assert_eq!(destination(&output, &source.tracks[0].parts[0][1]), (0, 1));
}

#[test]
/// 실제 피치 변화 전까지 핵심 선율을 유지하는지 검사합니다.
fn learned_core_keeps_the_melody_until_the_actual_pitch_change() {
    let source = score(&[
        &[(0, 48, 79, 12), (48, 96, 79, 12), (96, 144, 79, 12)],
        &[(0, 48, 76, 12), (48, 96, 76, 12), (96, 144, 74, 12)],
        &[(0, 144, 55, 12)],
        &[(0, 144, 43, 12)],
    ]);
    for gain in [Gain::None, Gain::Auto, Gain::Shift(-15)] {
        let output = fold_score(
            &source,
            &FoldOptions {
                tracks: 2,
                parts: 3,
                layout: Layout::Learned,
                gain,
                ..FoldOptions::default()
            },
        )
        .unwrap()
        .score;
        let core = &output.tracks[0].parts;
        for (on, pitch) in [(48, 79), (96, 74)] {
            assert!(
                core.iter()
                    .flatten()
                    .any(|n| n.on == on && n.pitch == pitch)
            );
        }
        assert_monophonic(&output);
    }
}

#[test]
/// 트랙 수별 핵심 배치의 공격 보존과 꼬리 길이를 검사합니다.
fn learned_partition_preserves_retained_attacks_and_bounds_tail_cuts_across_track_counts() {
    let mut source = score(&[
        &[(0, 192, 84, 8)],
        &[(0, 96, 76, 8), (96, 192, 74, 8)],
        &[(48, 144, 67, 8)],
        &[(0, 48, 60, 8), (96, 192, 62, 8)],
        &[(0, 144, 48, 8)],
        &[(48, 192, 36, 8)],
    ]);
    source.tracks[0].tempos = vec![(0, 120), (72, 80), (144, 135)];
    let original = (events(&source), serialize_mmi(&source));
    let source_notes = source.all_notes();
    let peak = source_notes
        .iter()
        .map(|note| {
            source_notes
                .iter()
                .filter(|other| other.on <= note.on && other.off > note.on)
                .count()
        })
        .max()
        .unwrap();
    for tracks in 1..=4 {
        for parts in 1..=3 {
            let options = FoldOptions {
                tracks,
                parts,
                layout: Layout::Learned,
                gain: Gain::None,
                ..FoldOptions::default()
            };
            let result = fold_score(&source, &options).unwrap();
            let reloaded = parse_mmi(&serialize_mmi(&result.score)).unwrap();
            if tracks * parts >= peak {
                assert_same_attacks_with_bounded_tails(&reloaded, &source);
            } else {
                // 학습 배치의 새 공격 우선 때문에 유지 집합은 Voices와 다를 수 있습니다.
                assert_attack_subset_with_bounded_tails(&reloaded, &source);
            }
            let retained = reloaded.all_notes();
            assert_eq!(events(&reloaded), events(&result.score));
            assert_eq!(reloaded.tempos(), source.tempos());
            assert_eq!(result.stats.total, source_notes.len());
            assert_eq!(result.stats.kept, retained.len());
            assert_eq!(result.stats.dropped, source_notes.len() - retained.len());
            let shortened = retained
                .iter()
                .filter(|note| {
                    source_notes.iter().any(|original| {
                        (original.on, original.pitch, original.vel)
                            == (note.on, note.pitch, note.vel)
                            && note.off < original.off
                    })
                })
                .count();
            assert_eq!(result.stats.truncated, shortened);
            assert_eq!(result.score.tracks.len(), tracks);
            assert!(result.score.tracks.iter().all(|t| t.parts.len() == parts));
            assert_monophonic(&result.score);
        }
    }
    assert_eq!((events(&source), serialize_mmi(&source)), original);
}

/// 긴 지속음 사이에 새 선율이 들어오는 악보를 만듭니다.
fn learned_tail_fixture() -> Score {
    let mut source = score(&[
        &[(0, 384, 48, 6)],
        &[(0, 384, 60, 10)],
        &[(0, 384, 72, 12)],
        &[(96, 144, 78, 8), (144, 192, 79, 9)],
        &[(96, 144, 82, 8), (144, 192, 83, 9)],
        &[(96, 144, 86, 8), (144, 192, 87, 9)],
    ]);
    source.tracks[0].tempos = vec![(0, 120), (96, 84), (168, 144)];
    source
}

#[test]
/// 새 공격이 오래된 꼬리를 대체하고 재시작하지 않는지 검사합니다.
fn learned_new_attacks_replace_old_core_tails_without_restarting_them() {
    let source = learned_tail_fixture();
    let original = (events(&source), serialize_mmi(&source));
    let result = fold_score(
        &source,
        &FoldOptions {
            layout: Layout::Learned,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    // 핵심 세 자리를 채운 지속음을 교체해야 뒤의 선율이 들어옵니다.
    for (on, pitch) in [(96, 86), (144, 87)] {
        assert!(
            result.score.tracks[0]
                .parts
                .iter()
                .flatten()
                .any(|n| n.on == on && n.pitch == pitch),
            "New melody attack {on}/{pitch} must enter the core"
        );
    }
    let cuts: Vec<_> = result
        .score
        .all_notes()
        .into_iter()
        .filter(|n| n.on == 0 && n.off < 384)
        .collect();
    assert!(
        !cuts.is_empty(),
        "A full core must release a long tail for the new melody"
    );
    assert!(
        cuts.iter().all(|n| [96, 144].contains(&n.off)),
        "Tails end only at a real new onset"
    );
    assert_same_attacks_with_bounded_tails(&result.score, &source);
    assert_eq!(result.stats.kept, 9);
    assert_eq!(result.stats.dropped, 0);
    assert_monophonic(&result.score);
    let reloaded = parse_mmi(&serialize_mmi(&result.score)).unwrap();
    assert_eq!(events(&reloaded), events(&result.score));
    assert_eq!(reloaded.tempos(), source.tempos());
    assert_eq!(reloaded.head, source.head);
    assert_eq!(reloaded.tail, source.tail);
    // 공격 집합 전체를 비교해 꼬리 재진입·복제·반주 이동도 검출합니다.
    assert_eq!((events(&source), serialize_mmi(&source)), original);
}

#[test]
/// 빈자리나 경쟁 없는 지속음을 불필요하게 자르지 않는지 검사합니다.
fn learned_leaves_uncontested_tails_and_available_core_space_unchanged() {
    for source in [
        score(&[&[(0, 384, 48, 8)], &[(0, 384, 60, 8)], &[(0, 384, 72, 8)]]),
        score(&[&[(0, 384, 48, 8)], &[(96, 144, 86, 8), (144, 192, 87, 8)]]),
    ] {
        let result = fold_score(
            &source,
            &FoldOptions {
                layout: Layout::Learned,
                gain: Gain::None,
                ..FoldOptions::default()
            },
        )
        .unwrap();
        assert_eq!(events(&result.score), events(&source));
        assert!(result.score.tracks[1].parts.iter().all(Vec::is_empty));
        assert_monophonic(&result.score);
    }
}

#[test]
/// 출력 음량 제한이 공격 배치와 꼬리 절단을 바꾸지 않는지 검사합니다.
fn learned_attack_placement_and_tail_cuts_ignore_output_gain_clipping() {
    let source = learned_tail_fixture();
    let original = (events(&source), serialize_mmi(&source));
    let mut baseline_placements = None;
    for gain in [Gain::None, Gain::Auto, Gain::Shift(-15), Gain::Shift(15)] {
        let options = FoldOptions {
            layout: Layout::Learned,
            gain,
            ..FoldOptions::default()
        };
        let result = fold_score(&source, &options).unwrap();
        let reference = fold_score(
            &source,
            &FoldOptions {
                layout: Layout::Voices,
                ..options
            },
        )
        .unwrap();
        assert_same_attacks_with_bounded_tails(&result.score, &reference.score);
        let placements = attack_placements(&result.score);
        if let Some(expected) = &baseline_placements {
            assert_eq!(
                &placements, expected,
                "Gain {gain:?} changed a part or cut position"
            );
        } else {
            baseline_placements = Some(placements);
        }
        let reloaded = parse_mmi(&serialize_mmi(&result.score)).unwrap();
        assert_eq!(events(&reloaded), events(&result.score));
        assert_eq!(reloaded.tempos(), source.tempos());
    }
    assert_eq!((events(&source), serialize_mmi(&source)), original);
}

#[test]
/// 주선율 복귀가 지속 반주를 끊지 않는지 검사합니다.
fn roles_primary_returns_after_a_rest_without_interrupting_sustained_accompaniment() {
    let source = score(&[&[(0, 24, 84, 8), (48, 72, 86, 8)], &[(0, 120, 48, 8)]]);
    let output = fold_two_parts(&source, Layout::Roles);
    let melody = &source.tracks[0].parts[0];
    assert_eq!(destination(&output, &melody[0]), (0, 0));
    assert_eq!(destination(&output, &melody[1]), (0, 0));
    assert_eq!(destination(&output, &source.tracks[1].parts[0][0]), (0, 1));
}

#[test]
/// 주선율 교체 밖에서 다른 성부의 연속성을 검사합니다.
fn roles_keep_non_primary_sources_continuous_away_from_melody_handoffs() {
    let source = score(&[
        &[(0, 192, 96, 12)],
        &[(0, 24, 72, 8), (24, 48, 60, 8), (48, 72, 74, 8)],
        &[(0, 24, 48, 8), (24, 48, 72, 8), (48, 72, 50, 8)],
    ]);
    let result = fold_score(
        &source,
        &FoldOptions {
            tracks: 1,
            parts: 3,
            layout: Layout::Roles,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert_eq!(events(&result.score), events(&source));
    assert_monophonic(&result.score);
    assert_eq!(
        destination(&result.score, &source.tracks[0].parts[0][0]),
        (0, 0)
    );
    for track in &source.tracks[1..] {
        let first = destination(&result.score, &track.parts[0][0]);
        assert_ne!(first, (0, 0));
        assert!(
            track.parts[0]
                .iter()
                .all(|n| destination(&result.score, n) == first)
        );
    }
}

#[test]
/// 불규칙한 틱 길이의 배치와 MMI 인코딩을 검사합니다.
fn irregular_exact_tick_lengths_survive_phrase_placement_and_mmi_encoding() {
    let mut source = score(&[
        &[(0, 13, 72, 8), (13, 26, 60, 8), (33, 46, 74, 8)],
        &[(6, 13, 84, 8), (13, 20, 86, 8), (26, 33, 80, 8)],
    ]);
    source.tracks[0].tempos = vec![(0, 120), (13, 83), (26, 137)];
    for layout in [
        Layout::Voices,
        Layout::Hands,
        Layout::Roles,
        Layout::Learned,
    ] {
        let output = fold_two_parts(&source, layout);
        let parsed = parse_mmi(&serialize_mmi(&output)).unwrap();
        assert_eq!(events(&parsed), events(&source));
        assert_eq!(parsed.tempos(), source.tempos());
    }
}

#[test]
/// 복귀 선율이 표현 불가능한 2틱 쉼표를 만들지 않는지 검사합니다.
fn a_returning_phrase_uses_an_exact_rest_instead_of_creating_a_two_tick_gap() {
    let source = score(&[
        &[
            (12, 36, 70, 8),
            (48, 72, 69, 8),
            (78, 89, 56, 8),
            (209, 222, 63, 8),
            (228, 241, 79, 8),
            (361, 367, 57, 8),
            (487, 506, 78, 8),
            (626, 650, 51, 8),
        ],
        &[
            (7, 18, 68, 8),
            (30, 54, 56, 8),
            (61, 74, 52, 8),
            (194, 213, 66, 8),
            (309, 315, 47, 8),
            (322, 335, 52, 8),
            (335, 354, 81, 8),
            (354, 365, 81, 8),
        ],
        &[
            (7, 18, 62, 8),
            (24, 31, 54, 8),
            (151, 158, 67, 8),
            (158, 165, 42, 8),
            (171, 177, 79, 8),
            (177, 188, 45, 8),
            (200, 207, 68, 8),
            (303, 327, 63, 8),
        ],
    ]);
    for layout in [Layout::Hands, Layout::Roles] {
        let result = fold_score(
            &source,
            &FoldOptions {
                tracks: 1,
                parts: 3,
                layout,
                gain: Gain::None,
                ..FoldOptions::default()
            },
        )
        .unwrap();
        assert_eq!(events(&result.score), events(&source));
        assert_monophonic(&result.score);
        let parsed = parse_mmi(&serialize_mmi(&result.score)).unwrap();
        assert_eq!(events(&parsed), events(&source));
    }
}

#[test]
/// 템포 조각 인코딩 실패 시 시각 보정 없이 기존 배치를 재시도하는지 검사합니다.
fn an_unencodable_tempo_fragment_retries_legacy_placement_without_rounding() {
    let mut source = score(&[
        &[(0, 24, 72, 8), (48, 72, 74, 8)],
        &[(24, 120, 67, 8)],
        &[(0, 120, 36, 8)],
    ]);
    source.tracks[0].meta.insert("name".into(), "Right".into());
    source.tracks[1].meta.insert("name".into(), "Right".into());
    source.tracks[2].meta.insert("name".into(), "Left".into());
    // 복귀 선율의 2틱 조각을 피하도록 정확히 나눌 수 있는 기존 반주에 템포를 둡니다.
    source.tracks[0].tempos = vec![(0, 120), (50, 83)];
    let result = fold_score(
        &source,
        &FoldOptions {
            tracks: 1,
            parts: 3,
            layout: Layout::Hands,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert!(result.report.contains("original interval packing"));
    assert_eq!(events(&result.score), events(&source));
    assert_eq!(result.score.tempos(), source.tempos());
    assert_eq!(result.stats.dropped, 0);
    assert_eq!(result.stats.truncated, 0);
    assert_monophonic(&result.score);
}

#[test]
/// 용량 이내에서 모든 공격과 길이를 유지하는지 검사합니다.
fn under_capacity_preserves_attacks_and_legacy_durations_for_every_part_count() {
    let source = score(&[
        &[(0, 48, 72, 9), (48, 96, 76, 11), (120, 168, 74, 10)],
        &[(0, 96, 48, 8), (96, 192, 50, 7)],
    ]);
    let before = events(&source);
    for layout in [
        Layout::Voices,
        Layout::Hands,
        Layout::Roles,
        Layout::Learned,
    ] {
        for parts in 1..=3 {
            let result = fold_score(
                &source,
                &FoldOptions {
                    tracks: 2,
                    parts,
                    layout,
                    gain: Gain::None,
                    ..FoldOptions::default()
                },
            )
            .unwrap();
            if layout == Layout::Learned {
                assert_same_attacks_with_bounded_tails(&result.score, &source);
            } else {
                assert_eq!(events(&result.score), before, "{layout:?}, {parts} parts");
                assert_eq!(result.stats.truncated, 0);
            }
            assert_eq!(result.stats.total, result.stats.kept);
            assert_eq!(result.stats.dropped, 0);
            assert_eq!(result.score.tracks.len(), 2);
            assert!(result.score.tracks.iter().all(|t| t.parts.len() == parts));
            assert_monophonic(&result.score);
        }
    }
    assert_eq!(events(&source), before, "source must remain immutable");
}

#[test]
/// 용량 초과 시 고유한 양끝 음보다 유니즌을 먼저 줄이는지 검사합니다.
fn over_capacity_removes_unison_before_distinct_extremes() {
    let source = score(&[
        &[(0, 96, 48, 8)],
        &[(0, 96, 60, 8)],
        &[(0, 96, 60, 8)],
        &[(0, 96, 72, 8)],
    ]);
    let result = fold_score(
        &source,
        &FoldOptions {
            tracks: 1,
            parts: 3,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        events(&result.score),
        vec![(0, 96, 48, 8), (0, 96, 60, 8), (0, 96, 72, 8)]
    );
    assert_eq!(result.stats.total, 4);
    assert_eq!(result.stats.kept, 3);
    assert_eq!(result.stats.dropped, 1);
    assert_eq!(result.stats.truncated, 0);
}

#[test]
/// 꼬리 절단이 원래 공격을 남기는지 검사합니다.
fn truncation_keeps_the_original_attack() {
    let source = score(&[&[(0, 192, 60, 8)], &[(48, 144, 84, 15)]]);
    let result = fold_score(
        &source,
        &FoldOptions {
            tracks: 1,
            parts: 1,
            layout: Layout::Voices,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        events(&result.score),
        vec![(0, 48, 60, 8), (48, 144, 84, 15)]
    );
    assert_eq!(result.stats.kept, 2);
    assert_eq!(result.stats.truncated, 1);
    assert_eq!(result.stats.dropped, 0);
    assert_eq!(events(&source)[0], (0, 192, 60, 8));
}

#[test]
/// 모든 배치 방식에서 빈 악보와 성긴 악보를 처리하는지 검사합니다.
fn empty_and_sparse_scores_work_with_all_layouts() {
    for source in [Score::default(), score(&[&[(0, 96, 60, 8)]])] {
        for layout in [
            Layout::Voices,
            Layout::Hands,
            Layout::Roles,
            Layout::Learned,
        ] {
            let result = fold_score(
                &source,
                &FoldOptions {
                    tracks: 16,
                    parts: 3,
                    layout,
                    gain: Gain::None,
                    ..FoldOptions::default()
                },
            )
            .unwrap();
            assert_eq!(events(&result.score), events(&source));
            assert_eq!(result.score.tracks.len(), 16);
            assert_monophonic(&result.score);
        }
    }
    let empty = fold_score(&Score::default(), &FoldOptions::default()).unwrap();
    assert_eq!(empty.stats.volume_shift, 0);
    assert!(empty.report.contains("no notes"));
}

#[test]
/// 자동 음량 이동과 수동 이동의 제한 보고를 검사합니다.
fn auto_gain_preserves_differences_and_manual_gain_reports_clipping() {
    let source = score(&[&[(0, 96, 60, 4), (96, 192, 62, 10)]]);
    let automatic = fold_score(&source, &FoldOptions::default()).unwrap();
    assert_eq!(automatic.stats.volume_shift, 5);
    assert_eq!(
        events(&automatic.score),
        vec![(0, 96, 60, 9), (96, 192, 62, 15)]
    );
    let manual = fold_score(
        &source,
        &FoldOptions {
            gain: Gain::Shift(12),
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert!(manual.score.all_notes().iter().all(|n| n.vel == 15));
    assert!(manual.report.contains("2 notes clipped"));
    assert!(!manual.report.contains("dynamic range unchanged"));
    let extreme = fold_score(
        &source,
        &FoldOptions {
            gain: Gain::Shift(i32::MIN),
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert!(extreme.score.all_notes().iter().all(|n| n.vel == 0));
}

#[test]
/// 짧은 지속음 교체에서 공격과 MMI 템포를 보존하는지 검사합니다.
fn eighth_note_handoff_preserves_attacks_and_mmi_tempo_roundtrip() {
    let mut source = score(&[
        &[(0, 12, 74, 10), (24, 36, 74, 10), (48, 60, 74, 10)],
        &[
            (0, 6, 50, 8),
            (6, 18, 55, 8),
            (18, 24, 57, 8),
            (24, 144, 62, 10),
        ],
        &[(0, 48, 50, 10)],
        &[(6, 48, 55, 10)],
        &[(18, 48, 57, 10)],
    ]);
    source.tracks[0].tempos = vec![(0, 120), (24, 144), (72, 132)];
    let original = source.all_notes();
    let result = fold_score(
        &source,
        &FoldOptions {
            layout: Layout::Learned,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    let core: Vec<_> = result.score.tracks[0].parts.iter().flatten().collect();
    assert!(
        core.iter()
            .any(|n| (n.on, n.off, n.pitch, n.vel) == (0, 24, 50, 10))
    );
    assert!(core.iter().any(|n| (n.on, n.off, n.pitch) == (24, 144, 62)));
    assert!(
        core.iter()
            .any(|n| (n.on, n.off, n.pitch, n.vel) == (6, 48, 55, 10))
    );
    assert_same_attacks_with_bounded_tails(&result.score, &source);
    assert_eq!(source.all_notes(), original);
    assert!(!result.report.to_lowercase().contains("fallback"));
    let reread = parse_mmi(&serialize_mmi(&result.score)).unwrap();
    assert_eq!(events(&reread), events(&result.score));
    assert_eq!(reread.tempos(), source.tempos());
    for track in &reread.tracks {
        for part in &track.parts {
            assert!(part.windows(2).all(|pair| pair[0].off <= pair[1].on));
        }
    }
}

#[test]
/// 같은 악보의 출력과 보고서가 결정적인지 검사합니다.
fn equal_scores_have_deterministic_output_and_reports() {
    let source = score(&[
        &[(0, 48, 60, 8), (48, 96, 62, 8)],
        &[(0, 48, 60, 8), (48, 96, 62, 8)],
        &[(0, 48, 60, 8), (48, 96, 62, 8)],
        &[(0, 48, 60, 8), (48, 96, 62, 8)],
    ]);
    for layout in [
        Layout::Voices,
        Layout::Hands,
        Layout::Roles,
        Layout::Learned,
    ] {
        let options = FoldOptions {
            tracks: 1,
            parts: 2,
            layout,
            ..FoldOptions::default()
        };
        let first = fold_score(&source, &options).unwrap();
        let second = fold_score(&source, &options).unwrap();
        assert_eq!(serialize_mmi(&first.score), serialize_mmi(&second.score));
        assert_eq!(first.report, second.report);
        assert_eq!(first.stats, second.stats);
    }
}

#[test]
/// 템포·헤더·박자표·악기 메타데이터 직렬화를 검사합니다.
fn tempo_header_tail_and_instrument_survive_serialization() {
    let mut source = score(&[&[(0, 96, 60, 8), (96, 192, 62, 8)]]);
    source.tracks[0].tempos = vec![(0, 120), (48, 144)];
    source.tracks[0].meta.insert("program".into(), "40".into());
    let result = fold_score(&source, &FoldOptions::default()).unwrap();
    let parsed = parse_mmi(&serialize_mmi(&result.score)).unwrap();
    assert_eq!(parsed.head, source.head);
    assert_eq!(parsed.tail, source.tail);
    for track in &parsed.tracks {
        assert_eq!(track.tempos, vec![(0, 120), (48, 144)]);
        assert_eq!(track.meta["program"], "40");
    }
}

#[test]
/// 지원하지 않는 옵션과 잘못된 음표의 오류를 검사합니다.
fn unsupported_options_and_invalid_notes_return_errors() {
    for options in [
        FoldOptions {
            tracks: 0,
            ..FoldOptions::default()
        },
        FoldOptions {
            tracks: 17,
            ..FoldOptions::default()
        },
        FoldOptions {
            parts: 0,
            ..FoldOptions::default()
        },
        FoldOptions {
            parts: 4,
            ..FoldOptions::default()
        },
        FoldOptions {
            lead_pitch: f64::NAN,
            ..FoldOptions::default()
        },
        FoldOptions {
            lead_continuity: -1.0,
            ..FoldOptions::default()
        },
    ] {
        assert!(fold_score(&Score::default(), &options).is_err());
    }
    for source in [
        score(&[&[(0, 0, 60, 8)]]),
        score(&[&[(-6, 6, 60, 8)]]),
        score(&[&[(0, 3, 60, 8)]]),
        score(&[&[(0, 96, 60, 16)]]),
        score(&[&[(0, 96, 60, 8), (48, 144, 62, 8)]]),
    ] {
        assert!(fold_score(&source, &FoldOptions::default()).is_err());
    }
}

#[test]
/// 6파트 실곡을 같은 용량으로 편곡할 때 모든 음을 보존하는지 검사합니다.
fn supplied_six_part_fixture_retains_every_note_at_same_capacity() {
    let source =
        load_mmi(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Op39No11_full_6part.mmi"))
            .unwrap();
    let result = fold_score(
        &source,
        &FoldOptions {
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    assert_eq!(events(&result.score), events(&source));
    assert_eq!(result.stats.total, result.stats.kept);
    assert_monophonic(&result.score);
}

#[test]
/// 오래된 여섯 지속음 사이의 옥타브 선율 공격을 검사합니다.
fn learned_reduction_keeps_moving_octave_attacks_between_six_old_tails() {
    for transpose in [-12, 0, 7] {
        for bpm in [96, 120] {
            let mut voices: Vec<Vec<_>> = [42, 49, 56, 63, 70, 77]
                .into_iter()
                .map(|pitch| vec![(0, 576, pitch + transpose, 12)])
                .collect();
            // 페달로 늘어난 음은 원본 파트가 달라도 새 공격으로 살아남아야 합니다.
            for (step, pitch) in [45, 47, 48, 50, 52, 50, 48, 47].into_iter().enumerate() {
                let on = 96 + step as i64 * 24;
                for octave in [0, 12] {
                    voices.push(vec![(on, on + 48, pitch + octave + transpose, 8)]);
                }
            }
            let slices: Vec<_> = voices.iter().map(Vec::as_slice).collect();
            let mut source = score(&slices);
            source.tracks[0].tempos = vec![(0, bpm), (192, bpm + 24)];
            let original = source.all_notes();
            let result = fold_score(
                &source,
                &FoldOptions {
                    tracks: 2,
                    parts: 3,
                    layout: Layout::Learned,
                    gain: Gain::None,
                    ..FoldOptions::default()
                },
            )
            .unwrap();
            // 새 공격은 최대 두 개이므로 오래된 꼬리를 줄여 모두 다음 단계로 보냅니다.
            assert_same_attacks_with_bounded_tails(&result.score, &source);
            assert_eq!(result.stats.kept, original.len());
            assert_eq!(result.stats.dropped, 0);
            let cuts: Vec<_> = result
                .score
                .all_notes()
                .into_iter()
                .filter(|note| note.on == 0 && note.off < 576)
                .collect();
            assert!(
                !cuts.is_empty(),
                "The full six-part output must release old tails"
            );
            assert!(
                cuts.iter()
                    .all(|note| { (96..=264).contains(&note.off) && (note.off - 96) % 24 == 0 })
            );
            assert_eq!(result.score.tracks.len(), 2);
            assert!(
                result
                    .score
                    .tracks
                    .iter()
                    .all(|track| track.parts.len() == 3)
            );
            assert_monophonic(&result.score);
            let restored = parse_mmi(&serialize_mmi(&result.score)).unwrap();
            assert_eq!(events(&restored), events(&result.score));
            assert_eq!(restored.tempos(), source.tempos());
            assert_eq!(source.all_notes(), original);
        }
    }
}

#[test]
/// 여섯 지속음 아래의 동음 재타격을 보존하는지 검사합니다.
fn learned_reduction_preserves_retriggers_under_six_sustained_voices() {
    for transpose in [-12, 0, 12] {
        let mut voices: Vec<Vec<_>> = [48, 55, 62, 69, 76, 83]
            .into_iter()
            .map(|pitch| vec![(0, 576, pitch + transpose, 12)])
            .collect();
        // 페달로 이전 음이 울려도 동음 재타격은 별개의 공격입니다.
        for step in 0..8 {
            let on = 96 + step * 24;
            voices.push(vec![(on, on + 192, 62 + transpose, 8)]);
        }
        let slices: Vec<_> = voices.iter().map(Vec::as_slice).collect();
        let source = score(&slices);
        let original = source.all_notes();
        let result = fold_score(
            &source,
            &FoldOptions {
                tracks: 2,
                parts: 3,
                layout: Layout::Learned,
                gain: Gain::None,
                ..FoldOptions::default()
            },
        )
        .unwrap();
        assert_same_attacks_with_bounded_tails(&result.score, &source);
        assert_eq!(result.stats.kept, 14);
        assert_eq!(result.stats.dropped, 0);
        assert_monophonic(&result.score);
        let restored = parse_mmi(&serialize_mmi(&result.score)).unwrap();
        assert_eq!(events(&restored), events(&result.score));
        assert_eq!(source.all_notes(), original);
    }
}

#[test]
/// 동시 화음을 제한하면서 뒤의 성긴 음은 유지하는지 검사합니다.
fn learned_reduction_still_limits_simultaneous_chords_and_keeps_later_sparse_notes() {
    let mut voices: Vec<Vec<_>> = [36, 43, 50, 57, 64, 71, 78, 85]
        .into_iter()
        .map(|pitch| vec![(0, 96, pitch, 8)])
        .collect();
    voices.push(vec![
        (192, 216, 60, 9),
        (216, 240, 62, 10),
        (264, 288, 64, 11),
    ]);
    voices.push(vec![(192, 288, 36, 7)]);
    let slices: Vec<_> = voices.iter().map(Vec::as_slice).collect();
    let source = score(&slices);
    let result = fold_score(
        &source,
        &FoldOptions {
            tracks: 2,
            parts: 3,
            layout: Layout::Learned,
            gain: Gain::None,
            ..FoldOptions::default()
        },
    )
    .unwrap();
    let output = events(&result.score);
    let source_events = events(&source);
    assert_eq!(output.iter().filter(|note| note.0 == 0).count(), 6);
    assert!(output.iter().all(|note| source_events.contains(note)));
    assert_eq!(
        output
            .iter()
            .filter(|note| note.0 >= 192)
            .collect::<Vec<_>>(),
        source_events
            .iter()
            .filter(|note| note.0 >= 192)
            .collect::<Vec<_>>()
    );
    assert_eq!(result.stats.kept, 10);
    assert_eq!(result.stats.dropped, 2);
    assert_eq!(result.stats.truncated, 0);
    assert_eq!(result.score.tracks.len(), 2);
    assert!(
        result
            .score
            .tracks
            .iter()
            .all(|track| track.parts.len() == 3)
    );
    assert_monophonic(&result.score);
}

#[test]
/// 표현 불가능한 템포 교체에서만 기존 축소 우선도를 재시도하는지 검사합니다.
fn learned_reduction_retries_original_priorities_only_for_an_unencodable_tempo_handoff() {
    let mut source = score(&[&[(0, 192, 84, 15)], &[(96, 192, 72, 1)]]);
    source.tracks[0].tempos = vec![(0, 120), (97, 144)];
    // 원본 템포 파트의 97/95틱과 다른 파트의 96틱 쉼표는 모두 표현 가능합니다.
    for track in &mut source.tracks {
        let part = mmlfold::core::emit_part(&track.parts[0], &track.tempos, 192).unwrap();
        track.mml = format!("MML@{part};");
    }
    let original = (events(&source), serialize_mmi(&source));
    let source = parse_mmi(&original.1).unwrap();
    assert_eq!(events(&source), original.0);
    assert_eq!(source.tempos(), vec![(0, 120), (97, 144)]);
    let before = serialize_mmi(&source);
    let marker = "reduction : original interval priorities to preserve exact MML timing";

    for parts in [1, 3] {
        let result = fold_score(
            &source,
            &FoldOptions {
                tracks: 1,
                parts,
                layout: Layout::Learned,
                gain: Gain::None,
                ..FoldOptions::default()
            },
        )
        .unwrap();
        if parts == 1 {
            // 96틱 교체는 유일한 템포 파트에 97틱까지 1틱 조각을 만듭니다.
            assert_eq!(events(&result.score), vec![(0, 192, 84, 15)]);
            assert_eq!(result.stats.kept, 1);
            assert_eq!(result.stats.dropped, 1);
            assert_eq!(result.stats.truncated, 0);
            assert!(result.report.contains(marker));
        } else {
            // 템포를 맡지 않는 파트에는 새 공격을 96틱 그대로 둘 수 있습니다.
            assert_eq!(events(&result.score), original.0);
            assert_eq!(result.stats.kept, 2);
            assert_eq!(result.stats.dropped, 0);
            assert_eq!(result.stats.truncated, 0);
            assert!(!result.report.contains(marker));
            assert!(!result.report.to_lowercase().contains("fallback"));
        }
        assert_eq!(result.score.tracks.len(), 1);
        assert_eq!(result.score.tracks[0].parts.len(), parts);
        assert_eq!(result.score.tempos(), source.tempos());
        assert_monophonic(&result.score);
        let restored = parse_mmi(&serialize_mmi(&result.score)).unwrap();
        assert_eq!(events(&restored), events(&result.score));
        assert_eq!(restored.tempos(), source.tempos());
    }
    assert_eq!(serialize_mmi(&source), before);
    assert_eq!(events(&source), original.0);
}
