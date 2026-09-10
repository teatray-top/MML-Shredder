//! 선율 진행과 재타격에서 베이스·화음 교체 조건을 검사합니다.

use super::*;

struct Fixture {
    notes: Vec<Note>,
    originals: Vec<Note>,
    probabilities: Vec<f64>,
    anchors: Vec<bool>,
}

// ID: 0=베이스, 1=첫 상성부, 2=최근 음, 3=새 음, 4=가까운 반주음, 5+=나머지.
/// 베이스와 선율이 화음으로 이어지는 테스트 자료를 만듭니다.
fn fixture(second: bool, transpose: i32, offset: Tick, descending: bool) -> Fixture {
    let (rows, probabilities) = if second {
        (
            vec![
                (0, 191, 42),
                (0, 191, 73),
                (71, 191, 76),
                (95, 191, 78),
                (95, 191, 58),
                (95, 191, 64),
            ],
            vec![0.969, 0.999, 0.971, 0.953, 0.001, 0.009],
        )
    } else {
        (
            vec![
                (0, 192, 47),
                (0, 192, 71),
                (72, 192, 74),
                (96, 192, 78),
                (96, 192, 54),
                (96, 192, 59),
                (96, 192, 62),
            ],
            vec![0.85, 0.985, 0.962, 0.916, 0.001, 0.004, 0.010],
        )
    };
    let mut originals: Vec<_> = rows
        .into_iter()
        .enumerate()
        .map(|(index, (on, off, pitch))| Note {
            on: on + offset,
            off: off + offset,
            pitch: pitch + transpose,
            vel: 8,
            src: (index, 0),
        })
        .collect();
    if descending {
        originals[1].pitch = 85 + transpose;
        originals[2].pitch = 82 + transpose;
        originals[3].pitch = 78 + transpose;
    }
    let mut notes = originals.clone();
    if !second {
        // 작업용 음만 사전 축소하고 원본의 긴 꼬리는 구절 판단에 남깁니다.
        notes[1].off = offset + 96;
    }
    let mut anchors = vec![false; notes.len()];
    anchors[3] = true;
    Fixture {
        notes,
        originals,
        probabilities,
        anchors,
    }
}

/// 공격 배치를 실행하고 음표·파트·MMI 재출력을 검사합니다.
fn run(fixture: &Fixture, tempos: &[Tempo]) -> (Vec<Note>, Vec<Vec<usize>>) {
    let mut notes = fixture.notes.clone();
    let kept: Vec<_> = (0..notes.len()).collect();
    let slots = schedule(
        &mut notes,
        &kept,
        &Policy {
            originals: &fixture.originals,
            probabilities: &fixture.probabilities,
            anchors: &fixture.anchors,
            parts: 3,
            count: 6,
            tempos,
        },
    )
    .unwrap();
    assert_eq!(slots.len(), 6);
    let mut assigned: Vec<_> = slots.iter().flatten().copied().collect();
    assigned.sort_unstable();
    assert_eq!(assigned, (0..notes.len()).collect::<Vec<_>>());
    for (index, note) in notes.iter().enumerate() {
        let original = &fixture.originals[index];
        assert_eq!(
            (note.on, note.pitch, note.vel, note.src),
            (original.on, original.pitch, original.vel, original.src)
        );
        assert!(note.off >= note.on + 6);
        assert!(note.off <= fixture.notes[index].off);
        assert!(note.off <= original.off);
    }
    for (slot, part) in slots.iter().enumerate() {
        assert!(
            part.windows(2)
                .all(|pair| notes[pair[0]].off <= notes[pair[1]].on)
        );
        let lane: Vec<_> = part.iter().map(|&index| notes[index].clone()).collect();
        let lane_tempos = if slot % 3 == 0 { tempos } else { &[] };
        let text = crate::core::emit_part(&lane, lane_tempos, 768).unwrap();
        let (decoded, _, _) = crate::core::parse_part(&text, (0, 0)).unwrap();
        let events = |notes: &[Note]| {
            notes
                .iter()
                .map(|note| (note.on, note.off, note.pitch, note.vel))
                .collect::<Vec<_>>()
        };
        assert_eq!(events(&decoded), events(&lane));
    }
    (notes, slots)
}

/// 지정한 시점에 핵심 트랙에서 울리는 음을 찾습니다.
fn active_core(
    notes: &[Note],
    slots: &[Vec<usize>],
    tick: Tick,
) -> std::collections::BTreeSet<usize> {
    slots[..3]
        .iter()
        .flatten()
        .copied()
        .filter(|&index| notes[index].on <= tick && notes[index].off > tick)
        .collect()
}

#[test]
/// 여러 음역에서 최근 선율과 가까운 반주음의 화음 교체를 검사합니다.
fn bass_to_chord_handoff_keeps_recent_upper_and_nearest_support_across_registers() {
    for second in [false, true] {
        for transpose in [-12, 0, 12] {
            for offset in [0, 384] {
                for descending in [false, true] {
                    let source = fixture(second, transpose, offset, descending);
                    let tick = source.notes[3].on;
                    let (notes, slots) = run(&source, &[(0, 120)]);
                    assert_eq!(
                        active_core(&notes, &slots, tick),
                        [2, 3, 4].into_iter().collect(),
                        "second={second}, transpose={transpose}, offset={offset}, descending={descending}"
                    );
                    assert_eq!(notes[0].off, tick);
                    assert_eq!(notes[1].off, tick);
                    assert_eq!(
                        notes[2], source.originals[2],
                        "Keep the recent upper intact"
                    );
                    for (note, original) in notes.iter().zip(&source.originals).skip(3) {
                        assert_eq!(note, original);
                    }
                    for index in 5..notes.len() {
                        assert!(slots[3..].iter().any(|part| part.contains(&index)));
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Guard {
    ShortSupport,
    WeakSupport,
    UnisonSupport,
    SamePitchUpper,
    SimultaneousUpper,
    DifferentBassStart,
    InsufficientRegisterGap,
    YoungHeldAttacks,
}

#[test]
/// 짧거나 약하거나 분산화음이 아닌 진입의 교체 제한을 검사합니다.
fn handoff_prior_does_not_override_short_weak_or_non_arpeggiated_entries() {
    for guard in [
        Guard::ShortSupport,
        Guard::WeakSupport,
        Guard::UnisonSupport,
        Guard::SamePitchUpper,
        Guard::SimultaneousUpper,
        Guard::DifferentBassStart,
        Guard::InsufficientRegisterGap,
        Guard::YoungHeldAttacks,
    ] {
        let mut source = fixture(true, 0, 0, false);
        for notes in [&mut source.notes, &mut source.originals] {
            match guard {
                Guard::ShortSupport => {
                    notes[4].off = notes[4].on + 48;
                    notes[5].off = notes[5].on + 48;
                }
                Guard::WeakSupport => {
                    notes[4].vel = 3;
                    notes[5].vel = 3;
                }
                Guard::UnisonSupport => notes[5].pitch = notes[4].pitch,
                Guard::SamePitchUpper => {
                    notes[1].pitch = 79;
                    notes[2].pitch = 79;
                    notes[3].pitch = 82;
                }
                Guard::SimultaneousUpper => notes[2].on = 0,
                Guard::DifferentBassStart => notes[1].on = 6,
                Guard::InsufficientRegisterGap => notes[5].pitch = 65,
                Guard::YoungHeldAttacks => {
                    notes[0].on = 72;
                    notes[1].on = 72;
                    notes[2].on = 83;
                }
            }
        }
        let (notes, slots) = run(&source, &[]);
        assert!(
            !active_core(&notes, &slots, source.notes[3].on).contains(&4),
            "Guard {guard:?} must not force the low-confidence support into the core"
        );
        if matches!(guard, Guard::YoungHeldAttacks) {
            assert_eq!(
                active_core(&notes, &slots, 95),
                [0, 1, 2].into_iter().collect()
            );
            assert_eq!(&notes[..3], &source.originals[..3]);
        }
    }
}

#[test]
/// 화음 교체가 정확한 템포 운반 가능성을 유지하는지 검사합니다.
fn handoff_prior_preserves_exact_tempo_carrier_feasibility() {
    let source = fixture(true, 0, 0, false);
    // 95틱 공격과 96틱 템포의 1틱 조각을 피하도록 기존 템포 파트를 유지합니다.
    let (notes, slots) = run(&source, &[(0, 120), (96, 144)]);
    assert!(!active_core(&notes, &slots, 95).contains(&4));
    assert_eq!(notes[1], source.originals[1]);
    assert_eq!(notes[3].on, 95);
    assert_eq!(notes[4].on, 95);
}

/// 같은 음의 재타격이 포함된 화음 교체 자료를 만듭니다.
fn repeated_fixture(
    transpose: i32,
    offset: Tick,
    ascending: bool,
    recent_still_held: bool,
) -> Fixture {
    let mut source = fixture(false, transpose, offset, false);
    let first_pitch = if ascending { 73 } else { 79 };
    let rows = [
        (0, 191, 42),
        (0, 191, first_pitch),
        (71, 191, 76),
        (95, 191, 76),
        (95, 191, 54),
        (95, 191, 58),
        (95, 191, 64),
    ];
    for (note, (on, off, pitch)) in source.originals.iter_mut().zip(rows) {
        note.on = on + offset;
        note.off = off + offset;
        note.pitch = pitch + transpose;
    }
    source.notes = source.originals.clone();
    source.notes[1].off = offset + 143;
    if !recent_still_held {
        source.notes[2].off = offset + 95;
    }
    source.probabilities = vec![
        0.984737, 0.998199, 0.936508, 0.517192, 0.001131, 0.000452, 0.009554,
    ];
    source
}

#[test]
/// 동음 재타격 교체에서 첫 상성부와 개별 공격을 유지하는지 검사합니다.
fn repeated_upper_handoff_preserves_first_upper_and_separate_attacks() {
    for transpose in [-12, 0, 12] {
        for offset in [0, 384] {
            for ascending in [false, true] {
                for recent_still_held in [false, true] {
                    let source = repeated_fixture(transpose, offset, ascending, recent_still_held);
                    let tick = source.notes[3].on;
                    let (notes, slots) = run(&source, &[(0, 120)]);
                    assert_eq!(
                        active_core(&notes, &slots, tick),
                        [1, 3, 4].into_iter().collect(),
                        "transpose={transpose}, offset={offset}, ascending={ascending}, recent_still_held={recent_still_held}"
                    );
                    assert_eq!(notes[0].off, tick);
                    assert_eq!(
                        notes[1], source.notes[1],
                        "Keep the existing first-upper tail"
                    );
                    assert_eq!(source.originals[1].off, offset + 191);
                    assert_eq!(notes[1].off, offset + 143);
                    assert_eq!(notes[2].on, offset + 71);
                    assert_eq!(notes[2].off, tick);
                    for (note, original) in notes.iter().zip(&source.originals).skip(3) {
                        assert_eq!(note, original);
                    }
                    for index in [5, 6] {
                        assert!(slots[3..].iter().any(|part| part.contains(&index)));
                    }
                    if !recent_still_held {
                        // 재출력·재파싱에서도 인접한 동음이 별도 공격이어야 합니다.
                        assert!(slots[..3].iter().any(|part| {
                            part.windows(2).any(|pair| pair[0] == 2 && pair[1] == 3)
                        }));
                    }
                }
            }
        }
    }
}

#[test]
/// 재타격 교체의 반주 길이를 새 음 길이로 판단하는지 검사합니다.
fn repeated_handoff_uses_fresh_duration_for_lower_support() {
    for (fresh_duration, support_duration, expected_handoff) in [
        (48, 48, true),
        (48, 24, false),
        (96, 96, true),
        (96, 48, false),
    ] {
        let mut source = repeated_fixture(0, 0, false, false);
        for notes in [&mut source.notes, &mut source.originals] {
            notes[3].off = notes[3].on + fresh_duration;
            for support in &mut notes[4..] {
                support.off = support.on + support_duration;
            }
        }
        // 이미 끝난 최근 음 대신 새 음 길이로 반주를 판단해야 48/48도 허용됩니다.
        assert_eq!(source.notes[2].off, source.notes[3].on);
        let (notes, slots) = run(&source, &[]);
        assert_eq!(
            active_core(&notes, &slots, 95).contains(&4),
            expected_handoff,
            "fresh={fresh_duration}, support={support_duration}"
        );
        assert_eq!(notes[1], source.notes[1]);
        if expected_handoff {
            assert_eq!(
                active_core(&notes, &slots, 95),
                [1, 3, 4].into_iter().collect()
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RepeatedGuard {
    FirstAlreadyEnded,
    DirectionReversal,
    RecentYoungerThanTwentyFourTicks,
}

#[test]
/// 종료된 상성부·역방향 진행·어린 공격에서 교체를 제한하는지 검사합니다.
fn repeated_handoff_does_not_revive_ended_upper_reverse_direction_or_cut_young_attack() {
    for guard in [
        RepeatedGuard::FirstAlreadyEnded,
        RepeatedGuard::DirectionReversal,
        RepeatedGuard::RecentYoungerThanTwentyFourTicks,
    ] {
        let mut source = repeated_fixture(0, 0, false, true);
        match guard {
            RepeatedGuard::FirstAlreadyEnded => {
                source.notes[1].off = 95;
                source.notes[2].off = 95;
            }
            RepeatedGuard::DirectionReversal => {
                source.notes[3].pitch = 78;
                source.originals[3].pitch = 78;
            }
            RepeatedGuard::RecentYoungerThanTwentyFourTicks => {
                source.notes[2].on = 77;
                source.originals[2].on = 77;
            }
        }
        let (notes, slots) = run(&source, &[]);
        assert!(
            !active_core(&notes, &slots, 95).contains(&4),
            "Guard {guard:?} must not force the lower support into the core"
        );
        assert_eq!(notes[1], source.notes[1]);
        if matches!(guard, RepeatedGuard::FirstAlreadyEnded) {
            assert!(!active_core(&notes, &slots, 95).contains(&1));
        } else {
            assert_eq!(notes[2], source.notes[2]);
        }
    }
}
