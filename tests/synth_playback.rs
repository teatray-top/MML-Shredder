//! 미리듣기의 시간·음색·음소거·탐색 동작을 검사합니다.

use std::collections::BTreeMap;
use std::sync::Arc;

use mmlfold::instruments::Instrument;
use mmlfold::synth::{Renderer, Timeline};
use mmlfold::{Note, Score, Track};

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

/// 음표나 MML 문자열로 테스트 악보를 만듭니다.
fn score(notes: Vec<Note>, tempos: Vec<(i64, i32)>) -> Score {
    Score {
        tracks: vec![Track {
            parts: vec![notes],
            tempos,
            ..Track::default()
        }],
        ..Score::default()
    }
}

/// 테스트 악보로 오디오 렌더러를 만듭니다.
fn renderer(score: &Score, sample_rate: u32) -> Renderer {
    Renderer::new(Arc::new(Timeline::from_score(score).unwrap()), sample_rate)
}

/// 두 실수가 허용 오차 이내인지 검사합니다.
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1.0e-9, "{actual} != {expected}");
}

/// 오디오 샘플의 에너지를 계산합니다.
fn energy(samples: &[[f32; 2]], channel: usize) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(sample[channel]).powi(2))
        .sum()
}

/// 지정한 프레임 수만큼 오디오를 생성합니다.
fn render_frames(audio: &mut Renderer, count: usize) -> Vec<[f32; 2]> {
    (0..count).map(|_| audio.next_frame()).collect()
}

/// 유한한 모노 출력에 소리가 있는지 검사합니다.
fn assert_audible_mono(samples: &[[f32; 2]]) {
    assert!(
        samples
            .iter()
            .all(|frame| { frame[0] == frame[1] && frame[0].is_finite() && frame[0].abs() <= 1.0 })
    );
    let rms = (energy(samples, 0) / samples.len() as f64).sqrt();
    assert!(rms > 1.0e-5, "inaudible instrument, RMS={rms}");
}

#[test]
/// 사용하지 않은 악기 변경이 기본 피아노 출력을 바꾸지 않는지 검사합니다.
fn default_piano_is_identical_to_explicit_piano_after_unused_program_changes() {
    let source = overlapping_parts();
    let mut default = renderer(&source, 48_000);
    let mut explicit = renderer(&source, 48_000);
    for (track, data) in source.tracks.iter().enumerate() {
        for part in 0..data.parts.len() {
            assert_eq!(default.part_instrument(track, part), Instrument::Piano);
            explicit.set_part_instrument(track, part, Instrument::Flute);
            explicit.set_part_instrument(track, part, Instrument::Piano);
        }
    }
    for _ in 0..48_000 {
        assert_eq!(default.next_frame(), explicit.next_frame());
    }
}

#[test]
/// 모든 내장 악기가 유한한 모노 음원을 내는지 검사합니다.
fn every_builtin_program_has_audible_finite_mono_output() {
    let source = score(vec![note(0, 192, 69, 12)], vec![]);
    let mut piano = renderer(&source, 48_000);
    let piano = render_frames(&mut piano, 24_000);
    for instrument in Instrument::ALL {
        let mut audio = renderer(&source, 48_000);
        audio.set_part_instrument(0, 0, instrument);
        let samples = render_frames(&mut audio, 24_000);
        assert_audible_mono(&samples);
        if instrument != Instrument::Piano {
            let difference: f64 = samples
                .iter()
                .zip(&piano)
                .map(|(actual, reference)| f64::from(actual[0] - reference[0]).powi(2))
                .sum();
            assert!(
                difference > 1.0e-4,
                "{instrument:?} still sounds like piano"
            );
        }
    }
}

/// 플루트와 현악기의 지속음 테스트 악보를 만듭니다.
fn flute_and_strings_score() -> Score {
    let mut source = score(vec![note(0, 768, 69, 12)], vec![]);
    // The same pitch and src metadata deliberately exercise part isolation.
    source.tracks[0].parts.push(vec![note(0, 768, 69, 12)]);
    source
}

/// 두 파트에 플루트와 현악기를 지정합니다.
fn flute_and_strings(source: &Score) -> Renderer {
    let mut audio = renderer(source, 48_000);
    audio.set_part_instrument(0, 0, Instrument::Flute);
    audio.set_part_instrument(0, 1, Instrument::Strings);
    audio
}

#[test]
/// 파트 음소거가 다른 악기의 지속음을 끊지 않는지 검사합니다.
fn flute_and_strings_part_gates_leave_the_other_held_voice_uninterrupted() {
    let source = flute_and_strings_score();
    let original = source.all_notes();
    let mut mixed = flute_and_strings(&source);
    let mut flute_only = flute_and_strings(&source);
    flute_only.set_part_enabled(0, 1, false);
    let mut strings_only = flute_and_strings(&source);
    strings_only.set_part_enabled(0, 0, false);
    for _ in 0..12_000 {
        mixed.next_frame();
        flute_only.next_frame();
        strings_only.next_frame();
    }
    for (flute_on, strings_on) in [(false, true), (true, false)] {
        let position = mixed.position_seconds();
        mixed.set_part_enabled(0, 0, flute_on);
        mixed.set_part_enabled(0, 1, strings_on);
        near(mixed.position_seconds(), position);
        // Let the existing 8ms mute/unmute ramp finish before comparing.
        for _ in 0..480 {
            mixed.next_frame();
            flute_only.next_frame();
            strings_only.next_frame();
        }
        let mut audible = Vec::with_capacity(12_000);
        for _ in 0..12_000 {
            let actual = mixed.next_frame();
            let flute = flute_only.next_frame();
            let strings = strings_only.next_frame();
            assert_eq!(actual, if flute_on { flute } else { strings });
            audible.push(actual);
        }
        assert_audible_mono(&audible);
    }
    assert_eq!(mixed.part_instrument(0, 0), Instrument::Flute);
    assert_eq!(mixed.part_instrument(0, 1), Instrument::Strings);
    assert_eq!(source.all_notes(), original);
}

#[test]
/// 탐색 후 악기·음소거·지속음 에너지 보존을 검사합니다.
fn seeking_keeps_programs_gates_and_sustained_instrument_energy() {
    let source = flute_and_strings_score();
    let mut continuous = flute_and_strings(&source);
    for _ in 0..30_000 {
        continuous.next_frame();
    }
    let mut seeked = flute_and_strings(&source);
    seeked.seek(0.625);
    near(seeked.position_seconds(), 0.625);
    assert_eq!(seeked.part_instrument(0, 0), Instrument::Flute);
    assert_eq!(seeked.part_instrument(0, 1), Instrument::Strings);
    assert_eq!(seeked.next_frame(), [0.0; 2]);
    continuous.next_frame();
    for _ in 1..384 {
        continuous.next_frame();
        seeked.next_frame();
    }
    let expected = render_frames(&mut continuous, 4800);
    let actual = render_frames(&mut seeked, 4800);
    assert_audible_mono(&actual);
    // 탐색 복원은 8프레임 블록 위상이 달라질 수 있어 샘플 대신 에너지를 비교합니다.
    let rms_ratio = (energy(&actual, 0) / energy(&expected, 0)).sqrt();
    assert!(
        (0.5..2.0).contains(&rms_ratio),
        "seek changed held-note RMS by a factor of {rms_ratio}"
    );
    near(seeked.position_seconds(), continuous.position_seconds());

    seeked.set_part_enabled(0, 0, false);
    seeked.set_part_enabled(0, 1, false);
    seeked.seek(1.25);
    assert_eq!(seeked.part_instrument(0, 0), Instrument::Flute);
    assert_eq!(seeked.part_instrument(0, 1), Instrument::Strings);
    assert!(!seeked.part_enabled(0, 0));
    assert!(!seeked.part_enabled(0, 1));
    assert!(
        render_frames(&mut seeked, 4800)
            .iter()
            .all(|frame| *frame == [0.0; 2])
    );
    seeked.set_part_enabled(0, 1, true);
    for _ in 0..480 {
        seeked.next_frame();
    }
    assert_audible_mono(&render_frames(&mut seeked, 4800));
}

#[test]
/// 4초를 넘는 하프 여운의 렌더링과 탐색을 검사합니다.
fn harp_release_past_four_seconds_survives_rendering_and_seeking() {
    // 하프 여운을 측정할 동안 무음 이벤트로 재생 시간을 확보합니다.
    let source = score(vec![note(0, 96, 48, 15), note(1920, 2016, 60, 0)], vec![]);
    let mut continuous = renderer(&source, 48_000);
    continuous.set_part_instrument(0, 0, Instrument::Harp);
    for _ in 0..240_000 {
        continuous.next_frame();
    }
    let mut seeked = renderer(&source, 48_000);
    seeked.set_part_instrument(0, 0, Instrument::Harp);
    seeked.seek(5.0);
    for _ in 0..384 {
        continuous.next_frame();
        seeked.next_frame();
    }
    let expected = render_frames(&mut continuous, 4800);
    let actual = render_frames(&mut seeked, 4800);
    assert!(
        energy(&expected, 0) > 1.0e-8,
        "the long harp release was cut"
    );
    assert!(
        energy(&actual, 0) > 1.0e-8,
        "seeking discarded the long harp release"
    );
    let ratio = (energy(&actual, 0) / energy(&expected, 0)).sqrt();
    assert!(
        (0.5..2.0).contains(&ratio),
        "harp release RMS ratio: {ratio}"
    );
    near(continuous.position_seconds(), seeked.position_seconds());
}

#[test]
/// 악기 변경이 기존 음을 유지하고 다음 공격부터 적용되는지 검사합니다.
fn instrument_changes_preserve_current_voices_and_apply_at_the_next_attack() {
    let source = score(vec![note(0, 288, 69, 12), note(384, 576, 69, 12)], vec![]);
    for (before, after) in [
        (Instrument::Piano, Instrument::Flute),
        (Instrument::Flute, Instrument::Strings),
    ] {
        let mut unchanged = renderer(&source, 48_000);
        let mut switched = renderer(&source, 48_000);
        unchanged.set_part_instrument(0, 0, before);
        switched.set_part_instrument(0, 0, before);
        for _ in 0..12_000 {
            assert_eq!(switched.next_frame(), unchanged.next_frame());
        }
        let position = switched.position_seconds();
        switched.set_part_instrument(0, 0, after);
        near(switched.position_seconds(), position);
        assert_eq!(switched.part_instrument(0, 0), after);
        // 다음 공격 전까지 기존 음과 여운의 음색을 유지해야 합니다.
        for _ in 12_000..96_000 {
            assert_eq!(switched.next_frame(), unchanged.next_frame());
        }
        near(switched.position_seconds(), 2.0);
        let actual = render_frames(&mut switched, 24_000);
        let expected = render_frames(&mut unchanged, 24_000);
        assert_audible_mono(&actual);
        let difference: f64 = actual
            .iter()
            .zip(&expected)
            .map(|(actual, reference)| f64::from(actual[0] - reference[0]).powi(2))
            .sum();
        assert!(
            difference > 1.0e-4,
            "new attack ignored program change {before:?} -> {after:?}"
        );
        near(switched.position_seconds(), unchanged.position_seconds());
    }
}

#[test]
/// 느린 템포·중복 템포·지속음 내부 템포의 정확성을 검사합니다.
fn exact_tempo_map_keeps_slow_tempos_last_duplicates_and_held_changes() {
    let source = score(
        vec![note(0, 384, 69, 15)],
        vec![(192, 240), (96, 60), (0, 120), (96, 6), (288, 30)],
    );
    let timeline = Timeline::from_score(&source).unwrap();
    near(timeline.tick_to_seconds(96.0), 0.5);
    near(timeline.tick_to_seconds(144.0), 5.5);
    near(timeline.tick_to_seconds(192.0), 10.5);
    near(timeline.tick_to_seconds(288.0), 10.75);
    near(timeline.duration_seconds(), 12.75);
    near(timeline.notes()[0].start, 0.0);
    near(timeline.notes()[0].end, 12.75);
    for quarter_tick in 0..=1536 {
        let tick = f64::from(quarter_tick) / 4.0;
        near(
            timeline.seconds_to_tick(timeline.tick_to_seconds(tick)),
            tick,
        );
    }
    near(timeline.tick_to_seconds(f64::INFINITY), 12.75);
    near(timeline.seconds_to_tick(f64::INFINITY), 384.0);
    near(timeline.tick_to_seconds(-1.0), 0.0);
    near(timeline.seconds_to_tick(f64::NAN), 0.0);
}

#[test]
/// 기본 템포·빈 악보·잘못된 음표 구간 처리를 검사합니다.
fn default_tempo_empty_score_and_invalid_intervals_are_handled() {
    let timeline = Timeline::from_score(&score(vec![note(96, 192, 60, 8)], vec![])).unwrap();
    near(timeline.notes()[0].start, 0.5);
    near(timeline.duration_seconds(), 1.0);
    let empty = Arc::new(Timeline::from_score(&Score::default()).unwrap());
    near(empty.tick_to_seconds(100.0), 0.0);
    near(empty.seconds_to_tick(100.0), 0.0);
    let mut empty_renderer = Renderer::new(empty, 48_000);
    assert!(empty_renderer.finished());
    assert_eq!(empty_renderer.next_frame(), [0.0; 2]);
    assert!(Timeline::from_score(&score(vec![note(-1, 96, 60, 8)], vec![])).is_err());
    assert!(Timeline::from_score(&score(vec![note(96, 0, 60, 8)], vec![])).is_err());
    assert!(Timeline::from_score(&score(vec![note(0, 96, 60, 8)], vec![(0, 0)])).is_err());
}

#[test]
/// 재생 데이터의 음표 순서·음량·원본 식별자 보존을 검사합니다.
fn note_order_velocity_and_source_metadata_are_preserved() {
    let mut source = score(vec![note(96, 192, 60, 0), note(0, 96, 69, 15)], vec![]);
    source.tracks[0].meta = BTreeMap::from([("panpot".into(), "0".into())]);
    source.tracks.push(Track {
        parts: vec![vec![note(0, 192, 72, 8)]],
        meta: BTreeMap::from([("panpot".into(), "127".into())]),
        ..Track::default()
    });
    let timeline = Timeline::from_score(&source).unwrap();
    assert_eq!(timeline.notes().len(), 3);
    assert_eq!(timeline.notes()[0].pitch, 69);
    assert_eq!(timeline.notes()[0].velocity, 1.0);
    assert_eq!(timeline.notes()[2].velocity, 0.0);
    assert_eq!(source.tracks[0].meta["panpot"], "0");
    assert_eq!(source.tracks[1].meta["panpot"], "127");
}

#[test]
/// 공격 시점의 샘플 정렬과 여운의 소멸을 검사합니다.
fn attacks_wait_for_their_sample_time_and_release_decays_to_silence() {
    let sample_rate = 48_000;
    let source = score(vec![note(96, 192, 69, 15)], vec![]);
    let mut audio = renderer(&source, sample_rate);
    for _ in 0..sample_rate / 2 {
        assert_eq!(audio.next_frame(), [0.0; 2], "sound before first attack");
    }
    near(audio.position_seconds(), 0.5);
    assert_eq!(audio.next_frame(), [0.0; 2]);
    let sounded: Vec<_> = (0..sample_rate / 2 - 1)
        .map(|_| audio.next_frame())
        .collect();
    assert!(energy(&sounded, 0) > 1.0);
    near(audio.position_seconds(), 1.0);
    assert!(
        !audio.finished(),
        "allow the released final note to ring briefly"
    );
    // The grand bank's documented release is about 0.8 seconds.
    let tail: Vec<_> = (0..sample_rate * 81 / 100)
        .map(|_| audio.next_frame())
        .collect();
    assert!(energy(&tail[..2400], 0) > energy(&tail[12_000..], 0) * 100.0);
    assert!(audio.finished());
    near(audio.position_seconds(), 1.0);
    for _ in 0..100 {
        assert_eq!(audio.next_frame(), [0.0; 2]);
    }
}

#[test]
/// 녹음된 A4의 조율과 음소거 출력을 검사합니다.
fn recorded_a4_keeps_concert_pitch_and_a_muted_note_is_silent() {
    let mut audio = renderer(&score(vec![note(0, 192, 69, 15)], vec![]), 48_000);
    for _ in 0..4800 {
        audio.next_frame();
    }
    let samples: Vec<_> = (0..4800).map(|_| audio.next_frame()[0]).collect();
    let component = |hz: f64| -> f64 {
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (index, sample) in samples.iter().enumerate() {
            let angle = std::f64::consts::TAU * hz * index as f64 / 48_000.0;
            real += f64::from(*sample) * angle.cos();
            imaginary += f64::from(*sample) * angle.sin();
        }
        real.hypot(imaginary)
    };
    // 실제 녹음의 배음·조율 편차를 허용하고 A4 기본 주파수를 검사합니다.
    let (peak_hz, peak) = (400..=480)
        .map(|hz| (hz, component(f64::from(hz))))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(
        (435..=445).contains(&peak_hz),
        "recorded A4 peaks at {peak_hz} Hz"
    );
    assert!(peak > component(400.0) * 3.0 && peak > component(480.0) * 3.0);
    let mut muted = renderer(&score(vec![note(0, 96, 69, 0)], vec![]), 48_000);
    for _ in 0..24_100 {
        assert_eq!(muted.next_frame(), [0.0; 2]);
    }
    assert!(muted.finished());
}

#[test]
/// 탐색 후 지속음·여운을 재타격 없이 복원하는지 검사합니다.
fn seek_restores_held_notes_and_release_without_retriggering_them() {
    let source = score(
        vec![
            note(0, 192, 69, 12),   // Held at seek.
            note(0, 96, 60, 10),    // Already releasing at seek.
            note(0, 48, 48, 15),    // Fully expired at seek.
            note(144, 240, 72, 11), // Future attack after seek.
        ],
        vec![],
    );
    let mut continuous = renderer(&source, 48_000);
    for _ in 0..30_000 {
        continuous.next_frame();
    }
    let mut seeked = renderer(&source, 48_000);
    seeked.seek(0.625);
    near(seeked.position_seconds(), 0.625);
    assert_eq!(
        seeked.next_frame(),
        [0.0; 2],
        "seek begins with an anti-click ramp"
    );
    continuous.next_frame();
    for _ in 1..384 {
        seeked.next_frame();
        continuous.next_frame();
    }
    let mut max_error = 0.0_f32;
    let mut audible_energy = 0.0;
    for _ in 0..15_000 {
        let expected = continuous.next_frame();
        let actual = seeked.next_frame();
        for channel in 0..2 {
            max_error = max_error.max((actual[channel] - expected[channel]).abs());
            audible_energy += f64::from(actual[channel]).powi(2);
        }
    }
    assert!(
        max_error < 1.0e-5,
        "seek differs from uninterrupted audio by {max_error}"
    );
    assert!(audible_energy > 1.0);
    seeked.seek(-100.0);
    near(seeked.position_seconds(), 0.0);
    seeked.seek(f64::INFINITY);
    near(seeked.position_seconds(), 1.25);
}

#[test]
/// 모노 재생의 팬 무시와 음량 범위를 검사합니다.
fn mono_ignores_track_and_keyboard_panning_and_keeps_dynamics_bounded() {
    let mut source = score(vec![note(0, 192, 69, 15)], vec![]);
    source.tracks[0].meta.insert("panpot".into(), "0".into());
    let mut left = renderer(&source, 48_000);
    let loud: Vec<_> = (0..4800).map(|_| left.next_frame()).collect();
    assert!(energy(&loud, 0) > 1.0);
    assert!(loud.iter().all(|frame| frame[0] == frame[1]));
    source.tracks[0].meta.insert("panpot".into(), "127".into());
    let mut right = renderer(&source, 48_000);
    let right: Vec<_> = (0..4800).map(|_| right.next_frame()).collect();
    assert_eq!(loud, right, "track panning must not change mono audio");
    for pitch in [24, 48, 72, 96] {
        let mut key = renderer(&score(vec![note(0, 96, pitch, 15)], vec![]), 48_000);
        for _ in 0..4800 {
            let frame = key.next_frame();
            assert_eq!(frame[0], frame[1], "keyboard pan at key {pitch}");
        }
    }
    source.tracks[0].meta.insert("panpot".into(), "0".into());
    source.tracks[0].parts[0][0].vel = 5;
    let mut soft = renderer(&source, 48_000);
    let soft: Vec<_> = (0..4800).map(|_| soft.next_frame()).collect();
    assert!(energy(&loud, 0) > energy(&soft, 0) * 8.0);
    let dense = score(
        (0..128).map(|n| note(0, 96, 36 + n % 60, 15)).collect(),
        vec![],
    );
    let mut dense = renderer(&dense, 48_000);
    let mut peak = 0.0_f32;
    for _ in 0..4800 {
        for sample in dense.next_frame() {
            assert!(sample.is_finite() && sample.abs() <= 1.0);
            peak = peak.max(sample.abs());
        }
    }
    assert!(peak > 0.3);
    let extreme = score(
        vec![note(0, 96, i32::MIN, 15), note(0, 96, i32::MAX, 15)],
        vec![],
    );
    let mut extreme = renderer(&extreme, 48_000);
    for _ in 0..1000 {
        assert_eq!(extreme.next_frame(), [0.0; 2]);
    }
}

#[test]
/// 한 트랙의 종료가 다른 트랙의 동음을 끊지 않는지 검사합니다.
fn ending_one_track_does_not_stop_another_tracks_same_pitch() {
    let mut source = score(vec![note(0, 96, 60, 15)], vec![]);
    source.tracks[0].meta.insert("panpot".into(), "0".into());
    source.tracks.push(Track {
        parts: vec![vec![note(0, 384, 60, 15)]],
        meta: BTreeMap::from([("panpot".into(), "127".into())]),
        ..Track::default()
    });
    let mut overlapping = renderer(&source, 48_000);
    // The first track's release is over, while the other track's key is held.
    overlapping.seek(1.35);
    let frames: Vec<_> = (0..4800).map(|_| overlapping.next_frame()).collect();
    assert!(frames.iter().all(|frame| frame[0] == frame[1]));
    assert!(energy(&frames, 0) + energy(&frames, 1) > 1.0);

    source.tracks.remove(0);
    let mut right_only = renderer(&source, 48_000);
    right_only.seek(1.35);
    for expected in frames {
        assert_eq!(right_only.next_frame(), expected);
    }
}

/// 같은 피치가 여러 파트에서 겹치는 악보를 만듭니다.
fn overlapping_parts() -> Score {
    let mut source = score(vec![note(0, 384, 60, 15)], vec![]);
    source.tracks[0]
        .parts
        .push(vec![note(0, 96, 60, 9), note(192, 384, 60, 12)]);
    // src가 같아도 음소거는 재배치된 트랙·파트 컨테이너를 따릅니다.
    source.tracks.push(Track {
        parts: vec![vec![note(48, 384, 60, 11)], Vec::new()],
        ..Track::default()
    });
    source
}

#[test]
/// 동음의 파트 음소거가 실제 출력 컨테이너를 따르는지 검사합니다.
fn independent_part_gates_follow_actual_containers_for_same_pitch_voices() {
    let source = overlapping_parts();
    let original = source.all_notes();
    let serialized = mmlfold::core::serialize_mmi(&source);
    let timeline = Timeline::from_score(&source).unwrap();
    assert_eq!(
        timeline
            .notes()
            .iter()
            .map(|n| (n.track, n.part))
            .collect::<Vec<_>>(),
        vec![(0, 0), (0, 1), (1, 0), (0, 1)]
    );
    let mut gated = renderer(&source, 48_000);
    gated.set_part_enabled(0, 1, false);
    assert!(gated.track_enabled(0));
    assert!(!gated.part_enabled(0, 1));
    assert!(
        gated.part_enabled(1, 1),
        "empty parts still have selections"
    );
    let mut expected_score = source.clone();
    for n in &mut expected_score.tracks[0].parts[1] {
        n.vel = 0;
    }
    let mut expected = renderer(&expected_score, 48_000);
    // Includes an initially muted attack, its release, and a future attack.
    for _ in 0..72_000 {
        let actual = gated.next_frame();
        assert_eq!(actual, expected.next_frame());
        assert_eq!(actual[0], actual[1]);
    }
    assert_eq!(source.all_notes(), original);
    assert_eq!(mmlfold::core::serialize_mmi(&source), serialized);
}

#[test]
/// 트랙 음소거의 파트 선택 유지와 다른 트랙 연속 재생을 검사합니다.
fn track_gate_preserves_part_choices_and_does_not_restart_other_tracks() {
    let source = overlapping_parts();
    let mut gated = renderer(&source, 48_000);
    gated.set_part_enabled(0, 1, false);
    gated.set_track_enabled(0, false);
    assert!(!gated.track_enabled(0));
    assert!(gated.part_enabled(0, 0));
    assert!(!gated.part_enabled(0, 1));

    let mut only_other_track = source.clone();
    for n in only_other_track.tracks[0].parts.iter_mut().flatten() {
        n.vel = 0;
    }
    let mut only_enabled_parts = source.clone();
    for n in &mut only_enabled_parts.tracks[0].parts[1] {
        n.vel = 0;
    }
    let mut other = renderer(&only_other_track, 48_000);
    let mut resumed = renderer(&only_enabled_parts, 48_000);
    for _ in 0..24_000 {
        assert_eq!(gated.next_frame(), other.next_frame());
        resumed.next_frame();
    }
    gated.set_track_enabled(0, true);
    assert!(!gated.part_enabled(0, 1));
    for _ in 0..480 {
        gated.next_frame();
        resumed.next_frame();
    }
    for _ in 0..24_000 {
        assert_eq!(gated.next_frame(), resumed.next_frame());
    }
    near(gated.position_seconds(), resumed.position_seconds());
}

#[test]
/// 실시간 음소거가 지속음·여운에 점진적으로 적용되는지 검사합니다.
fn live_gates_ramp_held_and_releasing_notes_without_retriggering_or_seeking() {
    for (off, elapsed_frames) in [(384, 24_000), (96, 30_000)] {
        let source = score(vec![note(0, off, 60, 15)], vec![]);
        let mut gated = renderer(&source, 48_000);
        let mut uninterrupted = renderer(&source, 48_000);
        gated.set_part_enabled(0, 0, false);
        for _ in 0..elapsed_frames {
            assert_eq!(gated.next_frame(), [0.0; 2]);
            uninterrupted.next_frame();
        }
        let position = gated.position_seconds();
        gated.set_part_enabled(0, 0, true);
        near(gated.position_seconds(), position);
        for frame in 0..480 {
            let expected = uninterrupted.next_frame();
            let actual = gated.next_frame();
            let ramp = ((frame + 1) as f64 / 384.0).min(1.0);
            assert!((f64::from(actual[0]) - f64::from(expected[0]) * ramp).abs() < 1.0e-7);
            assert_eq!(actual[0], actual[1]);
        }
        for _ in 0..480 {
            assert_eq!(gated.next_frame(), uninterrupted.next_frame());
        }
        gated.set_track_enabled(0, false);
        for frame in 0..480 {
            let expected = uninterrupted.next_frame();
            let actual = gated.next_frame();
            let ramp = (1.0 - (frame + 1) as f64 / 384.0).max(0.0);
            assert!((f64::from(actual[0]) - f64::from(expected[0]) * ramp).abs() < 1.0e-7);
        }
        assert_eq!(gated.next_frame(), [0.0; 2]);
        uninterrupted.next_frame();
        near(gated.position_seconds(), uninterrupted.position_seconds());
    }
}

#[test]
/// 전체 음소거 상태의 탐색·선택 유지·정상 종료를 검사합니다.
fn all_muted_seek_keeps_selections_and_reaches_normal_completion() {
    let source = overlapping_parts();
    let mut gated = renderer(&source, 48_000);
    gated.set_track_enabled(0, false);
    gated.set_part_enabled(1, 0, false);
    gated.seek(1.25);
    assert!(!gated.track_enabled(0));
    assert!(!gated.part_enabled(1, 0));
    assert!(gated.part_enabled(0, 1));
    assert!(!gated.finished());
    for _ in 0..96_000 {
        assert_eq!(gated.next_frame(), [0.0; 2]);
    }
    assert!(gated.finished());
    near(gated.position_seconds(), 2.0);
    gated.set_track_enabled(0, true);
    gated.set_part_enabled(1, 0, true);
    assert_eq!(
        gated.next_frame(),
        [0.0; 2],
        "expired notes do not retrigger"
    );
}

#[test]
/// 6파트 실곡의 재생 시간과 출력 신호를 검사합니다.
fn supplied_six_part_fixture_timing_and_audible_preview() {
    let source = mmlfold::core::parse_mmi(include_str!("../Op39No11_full_6part.mmi")).unwrap();
    let timeline = Arc::new(Timeline::from_score(&source).unwrap());
    assert_eq!(timeline.notes().len(), source.all_notes().len());
    assert_eq!(timeline.total_ticks(), source.total_ticks());
    // t6에서 12틱 페르마타는 1.25초입니다.
    near(
        timeline.tick_to_seconds(40_176.0) - timeline.tick_to_seconds(40_164.0),
        1.25,
    );
    // 정규화한 원본 템포를 별도로 적분해 전체 시간을 대조합니다.
    let mut events = BTreeMap::from([(0, 120)]);
    events.extend(source.tempos());
    let mut expected = 0.0;
    let mut previous_tick = 0;
    let mut bpm = 120;
    for (tick, next_bpm) in events {
        if tick > source.total_ticks() {
            break;
        }
        expected += (tick - previous_tick) as f64 * 60.0 / (96.0 * f64::from(bpm));
        previous_tick = tick;
        bpm = next_bpm;
    }
    expected += (source.total_ticks() - previous_tick) as f64 * 60.0 / (96.0 * f64::from(bpm));
    near(timeline.duration_seconds(), expected);
    let mut audio = Renderer::new(timeline.clone(), 48_000);
    let samples: Vec<_> = (0..24_000).map(|_| audio.next_frame()).collect();
    assert!(energy(&samples, 0) + energy(&samples, 1) > 1.0);
    audio.seek(timeline.tick_to_seconds(40_164.0));
    for _ in 0..4800 {
        assert!(
            audio
                .next_frame()
                .iter()
                .all(|s| s.is_finite() && s.abs() <= 1.0)
        );
    }
}

#[test]
#[ignore = "manual release-mode audio throughput measurement"]
/// 실곡 미리듣기의 렌더링 속도를 측정합니다.
fn fixture_render_speed() {
    let source = mmlfold::core::parse_mmi(include_str!("../Op39No11_full_6part.mmi")).unwrap();
    for mixed_instruments in [false, true] {
        let mut audio = renderer(&source, 48_000);
        if mixed_instruments {
            let mut index = 1;
            for (ti, track) in source.tracks.iter().enumerate() {
                for pi in 0..track.parts.len() {
                    audio.set_part_instrument(
                        ti,
                        pi,
                        Instrument::ALL[index % Instrument::ALL.len()],
                    );
                    index += 1;
                }
            }
        }
        let started = std::time::Instant::now();
        let mut energy = 0.0_f64;
        for _ in 0..48_000 * 20 {
            let frame = std::hint::black_box(audio.next_frame());
            energy += f64::from(frame[0]).powi(2) + f64::from(frame[1]).powi(2);
        }
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "Mixed instruments={mixed_instruments}: rendered 20.00 seconds at 48 kHz in {elapsed:.3} seconds ({:.1}x realtime), energy {energy:.3}",
            20.0 / elapsed
        );
        assert!(energy > 1.0);
        let before_seek = std::time::Instant::now();
        audio.seek(120.0);
        println!(
            "Seek to 120 seconds: {:.3}s",
            before_seek.elapsed().as_secs_f64()
        );
    }
}
