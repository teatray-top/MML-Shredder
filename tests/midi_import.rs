//! MIDI 시간·페달 처리와 MMI 변환을 검사합니다.

use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use mmlfold::{
    Score, core,
    fold::{FoldOptions, Gain, Layout},
    input, midi,
    split::SplitOptions,
    synth::Timeline,
    workflow,
};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// MIDI NoteOn 이벤트를 만듭니다.
fn on(channel: u8, pitch: u8, velocity: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: channel.into(),
        message: MidiMessage::NoteOn {
            key: pitch.into(),
            vel: velocity.into(),
        },
    }
}

/// MIDI 컨트롤 변경 이벤트를 만듭니다.
fn cc(channel: u8, controller: u8, value: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: channel.into(),
        message: MidiMessage::Controller {
            controller: controller.into(),
            value: value.into(),
        },
    }
}

/// MIDI 템포 이벤트를 만듭니다.
fn tempo(micros: u32) -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::Tempo(micros.into()))
}

/// MIDI 트랙 종료 이벤트를 만듭니다.
fn end() -> TrackEventKind<'static> {
    TrackEventKind::Meta(MetaMessage::EndOfTrack)
}

/// 테스트용 트랙을 MIDI 바이트로 직렬화합니다.
fn midi_bytes(
    format: Format,
    ppqn: u16,
    tracks: Vec<Vec<(u32, TrackEventKind<'static>)>>,
) -> Vec<u8> {
    let tracks = tracks
        .into_iter()
        .map(|mut events| {
            events.sort_by_key(|event| event.0);
            let mut previous = 0;
            events
                .into_iter()
                .map(|(tick, kind)| {
                    let event = TrackEvent {
                        delta: (tick - previous).into(),
                        kind,
                    };
                    previous = tick;
                    event
                })
                .collect()
        })
        .collect();
    let mut bytes = Vec::new();
    Smf {
        header: Header::new(format, Timing::Metrical(ppqn.into())),
        tracks,
    }
    .write_std(&mut bytes)
    .unwrap();
    bytes
}

/// 음표를 시작·끝·피치·음량 튜플로 모읍니다.
fn note_keys(score: &Score) -> Vec<(i64, i64, i32, i32)> {
    let mut keys: Vec<_> = score
        .all_notes()
        .iter()
        .map(|n| (n.on, n.off, n.pitch, n.vel))
        .collect();
    keys.sort();
    keys
}

/// MMI 저장과 재파싱 후 음표·템포 일치를 검사합니다.
fn assert_round_trip(score: &Score) {
    let restored = core::parse_mmi(&core::serialize_mmi(score)).unwrap();
    assert_eq!(note_keys(score), note_keys(&restored));
    assert_eq!(score.tempos(), restored.tempos());
    for (ti, track) in score.tracks.iter().enumerate() {
        assert_eq!(track.parts.len(), 3);
        for (pi, part) in track.parts.iter().enumerate() {
            assert!(part.windows(2).all(|pair| pair[0].off <= pair[1].on));
            assert!(part.iter().all(|n| n.src == (ti, pi)));
        }
    }
}

#[test]
/// PPQN 변환의 셋잇단음표 보존과 누적 반올림 오차를 검사합니다.
fn ppqn_conversion_preserves_triplets_and_does_not_accumulate_rounding() {
    for ppqn in [96u16, 384, 480, 960, 1000] {
        let mut events = vec![(0, tempo(500_000))];
        for i in 0..120u32 {
            let start = (i * u32::from(ppqn) + 1) / 3;
            let stop = ((i + 1) * u32::from(ppqn) + 1) / 3;
            events.push((start, on(0, 60 + (i % 12) as u8, 127)));
            events.push((stop, on(0, 60 + (i % 12) as u8, 0)));
        }
        events.push((u32::from(ppqn) * 40, end()));
        let score = midi::parse_midi(&midi_bytes(Format::SingleTrack, ppqn, vec![events])).unwrap();
        let keys = note_keys(&score);
        assert_eq!(keys.len(), 120);
        for (i, note) in keys.iter().enumerate() {
            assert_eq!(
                (note.0, note.1),
                (i as i64 * 32, (i as i64 + 1) * 32),
                "PPQN {ppqn}"
            );
            assert_eq!(note.3, 15);
        }
        assert_round_trip(&score);
    }
}

#[test]
/// 지휘 트랙 템포가 모든 트랙과 지속음에 적용되는지 검사합니다.
fn conductor_tempo_applies_to_all_tracks_and_crosses_held_notes() {
    let bytes = midi_bytes(
        Format::Parallel,
        480,
        vec![
            vec![
                (0, tempo(500_000)),
                (480, tempo(750_000)),
                (960, tempo(1_000_000)),
                (1440, end()),
            ],
            vec![(0, on(0, 60, 100)), (1440, on(0, 60, 0)), (1440, end())],
            vec![(480, on(1, 72, 100)), (960, on(1, 72, 0)), (1440, end())],
        ],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    assert_eq!(score.tempos(), vec![(0, 120), (96, 80), (192, 60)]);
    let timeline = Timeline::from_score(&score).unwrap();
    assert!((timeline.tick_to_seconds(score.total_ticks() as f64) - 2.25).abs() < 1e-9);
    let keys = note_keys(&score);
    assert!(keys.contains(&(0, 288, 60, 12)));
    assert!(keys.contains(&(96, 192, 72, 12)));
    assert_round_trip(&score);
}

#[test]
/// 분수 BPM의 긴 악보에서 실제 경과 시간을 보존하는지 검사합니다.
fn fractional_bpm_keeps_wall_time_over_a_long_score() {
    let micros = 498_000;
    let quarters = 1200;
    let bytes = midi_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, tempo(micros)),
            (0, on(0, 60, 127)),
            (quarters * 480, on(0, 60, 0)),
            (quarters * 480, end()),
        ]],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    let seconds = Timeline::from_score(&score)
        .unwrap()
        .tick_to_seconds(score.total_ticks() as f64);
    let expected = f64::from(quarters) * f64::from(micros) / 1_000_000.0;
    assert!(
        (seconds - expected).abs() < 0.02,
        "expected {expected}, got {seconds}"
    );
    assert_round_trip(&score);
}

#[test]
/// 트랙 간 공유 페달과 동음 재타격 보존을 검사합니다.
fn sustain_is_shared_by_channel_across_tracks_and_keeps_repeated_attacks() {
    let bytes = midi_bytes(
        Format::Parallel,
        480,
        vec![
            vec![(0, cc(0, 64, 127)), (960, cc(0, 64, 0)), (1440, end())],
            vec![
                (0, on(0, 60, 127)),
                (240, on(0, 60, 0)),
                (480, on(0, 60, 127)),
                (720, on(0, 60, 0)),
                (1440, end()),
            ],
            vec![(0, on(1, 67, 127)), (240, on(1, 67, 0)), (1440, end())],
        ],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    let keys = note_keys(&score);
    assert_eq!(
        keys,
        vec![(0, 48, 67, 15), (0, 192, 60, 15), (96, 192, 60, 15)]
    );
    assert_round_trip(&score);
}

#[test]
/// 동음 NoteOff가 다른 원본 트랙의 음을 끝내지 않는지 검사합니다.
fn same_key_note_offs_do_not_end_the_wrong_source_track() {
    let bytes = midi_bytes(
        Format::Parallel,
        480,
        vec![
            vec![(0, on(0, 60, 127)), (960, on(0, 60, 0)), (960, end())],
            vec![(240, on(0, 60, 64)), (480, on(0, 60, 0)), (960, end())],
        ],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    assert_eq!(note_keys(&score), vec![(0, 192, 60, 15), (48, 96, 60, 8)]);
    assert_round_trip(&score);
}

#[test]
/// 다른 MIDI 포트의 같은 채널이 페달을 공유하지 않는지 검사합니다.
fn identical_channels_on_different_midi_ports_do_not_share_pedal() {
    let port = |value: u8| TrackEventKind::Meta(MetaMessage::MidiPort(value.into()));
    let bytes = midi_bytes(
        Format::Parallel,
        480,
        vec![
            vec![
                (0, port(0)),
                (0, cc(0, 64, 127)),
                (960, cc(0, 64, 0)),
                (960, end()),
            ],
            vec![
                (0, port(0)),
                (0, on(0, 60, 127)),
                (240, on(0, 60, 0)),
                (960, end()),
            ],
            vec![
                (0, port(1)),
                (0, on(0, 60, 127)),
                (240, on(0, 60, 0)),
                (960, end()),
            ],
        ],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    assert_eq!(note_keys(&score), vec![(0, 48, 60, 15), (0, 192, 60, 15)]);
    assert_round_trip(&score);
}

#[test]
/// 짧은 공격을 살리면서 인코딩 불가능한 템포 조각을 피하는지 검사합니다.
fn sub_resolution_attacks_survive_without_unencodable_tempo_fragments() {
    let bytes = midi_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            (0, on(0, 60, 127)),
            (1, on(0, 60, 0)),
            (2, on(0, 62, 127)),
            (3, tempo(750_000)),
            (4, on(0, 62, 0)),
            (80, on(0, 64, 127)),
            (90, on(0, 64, 0)),
            (100, end()),
        ]],
    );
    let score = midi::parse_midi(&bytes).unwrap();
    assert_eq!(score.all_notes().len(), 3);
    assert!(
        score
            .all_notes()
            .iter()
            .all(|n| n.duration() >= core::MIN_TICK)
    );
    assert_round_trip(&score);
}

#[test]
/// 다성 MIDI의 입력 보존과 후속 편곡·분할을 검사합니다.
fn polyphonic_midi_is_packed_without_losing_notes_then_can_be_arranged_and_split() {
    let mut events = vec![
        (0, tempo(500_000)),
        (
            0,
            TrackEventKind::Meta(MetaMessage::TrackName(b"Piano\nmml-track=bad\r\n")),
        ),
        (
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
        ),
    ];
    for chord in 0..32 {
        for pitch in [36, 48, 60, 64, 67, 72, 79] {
            events.push((chord * 480, on(0, pitch, 100)));
            events.push(((chord + 1) * 480, on(0, pitch, 0)));
        }
    }
    events.push((32 * 480, end()));
    let score = midi::parse_midi(&midi_bytes(Format::SingleTrack, 480, vec![events])).unwrap();
    assert_eq!(score.all_notes().len(), 32 * 7);
    assert_eq!(score.tracks.len(), 3);
    assert!(score.tail.iter().any(|line| line == "0=3/4"));
    assert_round_trip(&score);
    let original = workflow::source_export(&score, "piano").unwrap();
    assert_eq!(original.artifacts.len(), 1);
    assert_eq!(original.artifacts[0].name, "piano.mmi");
    assert!(original.report.is_empty());
    let folded = workflow::arrange(
        &score,
        &FoldOptions {
            tracks: 2,
            parts: 3,
            gain: Gain::None,
            layout: Layout::Hands,
            ..FoldOptions::default()
        },
        "piano",
        false,
    )
    .unwrap();
    assert_eq!(folded.score.tracks.len(), 2);
    assert_round_trip(&folded.score);
    let split = workflow::scrolls(
        &folded.score,
        &SplitOptions {
            limit: 24,
            ..SplitOptions::default()
        },
        "piano",
    )
    .unwrap();
    assert!(split.artifacts.len() > 1);
    for artifact in split.artifacts {
        let chunk = core::parse_mmi(&artifact.text).unwrap();
        assert_eq!(chunk.tracks.len(), 2);
        assert_round_trip(&chunk);
    }
}

#[test]
/// 잘못된 MIDI와 독립 시퀀스 형식의 오류를 검사합니다.
fn malformed_and_independent_sequence_midi_are_rejected() {
    let bytes = midi_bytes(
        Format::SingleTrack,
        480,
        vec![vec![(0, on(0, 60, 127)), (480, on(0, 60, 0)), (480, end())]],
    );
    assert!(midi::parse_midi(&bytes[..bytes.len() - 7]).is_err());
    assert!(midi::parse_midi(b"not a MIDI file").is_err());
    let sequential = midi_bytes(
        Format::Sequential,
        480,
        vec![vec![(0, on(0, 60, 127)), (480, on(0, 60, 0)), (480, end())]],
    );
    assert!(midi::parse_midi(&sequential).is_err());
}

struct TempDir(PathBuf);
impl TempDir {
    /// 테스트용 임시 폴더를 만듭니다.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mmlfold-midi-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    /// 테스트에서 만든 임시 폴더를 정리합니다.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
/// 공통 로더와 CLI의 MIDI 입력 및 원본 보호를 검사합니다.
fn common_loader_and_cli_accept_midi_and_export_protected_mmi() {
    let dir = TempDir::new();
    let bytes = midi_bytes(
        Format::SingleTrack,
        480,
        vec![vec![(0, on(0, 60, 127)), (480, on(0, 60, 0)), (480, end())]],
    );
    for name in ["music.mid", "music.MID", "music.midi", "music.MIDI"] {
        let path = dir.0.join(name);
        fs::write(&path, &bytes).unwrap();
        assert!(input::is_midi_path(&path));
        assert_eq!(
            note_keys(&input::load_score(&path).unwrap()),
            vec![(0, 96, 60, 15)]
        );
    }
    let src = dir.0.join("music.mid");
    let out = dir.0.join("music.mmi");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_mmlfold"))
        .arg("import")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        note_keys(&input::load_score(&out).unwrap()),
        vec![(0, 96, 60, 15)]
    );
    let protected = std::process::Command::new(env!("CARGO_BIN_EXE_mmlfold"))
        .arg("import")
        .arg(&src)
        .arg("-o")
        .arg(&src)
        .arg("--force")
        .output()
        .unwrap();
    assert!(!protected.status.success());
    assert_eq!(fs::read(&src).unwrap(), bytes);
    assert!(!dir.0.join("music_report.txt").exists());
}
