//! MIDI 이벤트의 절대 시간을 보정해 MMI 악보로 변환합니다.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, ensure};
use midly::{Format, Fps, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::{
    Note, Score, Tempo, Tick,
    core::{MIN_TICK, emit_part, track_from_mml},
};

#[derive(Clone, Debug)]
struct MidiNote {
    on: u64,
    off: Option<u64>,
    track: usize,
    port: u8,
    channel: u8,
    pitch: u8,
    velocity: u8,
    program: u8,
}

struct Channel {
    keys: [VecDeque<usize>; 128],
    sustained: Vec<usize>,
    pedal: bool,
    program: u8,
}

impl Default for Channel {
    /// MIDI 채널의 건반과 페달 상태를 초기화합니다.
    fn default() -> Self {
        Self {
            keys: std::array::from_fn(|_| VecDeque::new()),
            sustained: Vec::new(),
            pedal: false,
            program: 0,
        }
    }
}

impl Channel {
    /// 페달 상태에 따라 음을 종료하거나 잔향 목록에 보관합니다.
    fn release(&mut self, index: usize, time: u64, notes: &mut [MidiNote]) {
        if self.pedal {
            self.sustained.push(index);
        } else {
            notes[index].off = Some(time);
        }
    }

    /// 원본 트랙을 우선해 대응하는 건반을 놓습니다.
    fn note_off(&mut self, track: usize, pitch: u8, time: u64, notes: &mut [MidiNote]) {
        let queue = &mut self.keys[usize::from(pitch)];
        // 같은 채널을 쓰는 손별 트랙을 구분하되 대응 건반이 없을 때만 다른 트랙의 note-off를 받습니다.
        let position = queue
            .iter()
            .position(|&i| notes[i].track == track)
            .or_else(|| (!queue.is_empty()).then_some(0));
        if let Some(index) = position.and_then(|position| queue.remove(position)) {
            self.release(index, time, notes);
        }
    }

    /// 페달을 놓고 보류된 음들을 종료합니다.
    fn release_pedal(&mut self, time: u64, notes: &mut [MidiNote]) {
        self.pedal = false;
        for index in self.sustained.drain(..) {
            notes[index].off = Some(time);
        }
    }

    /// 채널의 모든 건반을 놓거나 즉시 소리를 종료합니다.
    fn all_notes_off(&mut self, time: u64, notes: &mut [MidiNote], immediate: bool) {
        if immediate {
            // CC120은 소리만 끄고 페달 컨트롤러는 초기화하지 않습니다.
            for index in self.sustained.drain(..) {
                notes[index].off = Some(time);
            }
        }
        for queue in &mut self.keys {
            for index in queue.drain(..) {
                if self.pedal && !immediate {
                    self.sustained.push(index);
                } else {
                    notes[index].off = Some(time);
                }
            }
        }
    }
}

/// PPQN 또는 SMPTE 시간의 Type 0·1 MIDI를 MMI 악보로 변환합니다.
pub fn parse_midi(bytes: &[u8]) -> Result<Score> {
    ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "MIDI 파일이 너무 큽니다 (최대 64 MiB)."
    );
    let smf = Smf::parse(bytes)
        .map_err(|error| anyhow::anyhow!("MIDI 파일을 읽을 수 없습니다: {error}"))?;
    ensure!(
        smf.header.format != Format::Sequential,
        "MIDI Type 2는 서로 독립된 곡입니다. Type 0 또는 Type 1 MIDI로 저장해 주세요."
    );
    ensure!(!smf.tracks.is_empty(), "MIDI 파일에 트랙이 없습니다.");
    ensure!(
        smf.tracks.iter().map(Vec::len).sum::<usize>() <= 2_000_000,
        "MIDI 이벤트가 너무 많습니다 (최대 2,000,000개)."
    );
    ensure!(
        smf.header.format != Format::SingleTrack || smf.tracks.len() == 1,
        "MIDI Type 0 파일에 여러 트랙이 들어 있습니다."
    );
    let mut clock = SourceClock::new(smf.header.timing)?;

    let mut events = Vec::new();
    let mut end = 0_u64;
    let mut names = vec![String::new(); smf.tracks.len()];
    for (track_index, track) in smf.tracks.iter().enumerate() {
        let mut absolute = 0_u64;
        for (order, event) in track.iter().enumerate() {
            absolute = absolute
                .checked_add(u64::from(event.delta.as_int()))
                .context("MIDI 시간 범위를 초과했습니다.")?;
            end = end.max(absolute);
            events.push((absolute, track_index, order, event.kind));
        }
    }
    events.sort_by_key(|event| (event.0, event.1, event.2));

    let mut channels: BTreeMap<(u8, u8), Channel> = BTreeMap::new();
    let mut track_ports = vec![0; smf.tracks.len()];
    let mut notes = Vec::<MidiNote>::new();
    let mut tempos = BTreeMap::from([(0_u64, 500_000_u32)]);
    let mut signatures = BTreeMap::from([(0_u64, (4_u8, 4_u32))]);
    for (time, track, _, kind) in events {
        match kind {
            TrackEventKind::Midi { channel, message } => {
                let channel_index = channel.as_int();
                let port = track_ports[track];
                let state = channels.entry((port, channel_index)).or_default();
                match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        ensure!(
                            notes.len() < 1_000_000,
                            "MIDI 음표가 너무 많습니다 (최대 1,000,000개)."
                        );
                        let pitch = key.as_int();
                        // o0c 아래 C-와 o8b 위 B+를 포함하므로 MIDI 11~120까지 표현할 수 있습니다.
                        ensure!(
                            (11..=120).contains(&pitch),
                            "MIDI 트랙 {}의 음높이 {}는 MML로 표현할 수 없습니다 (지원 범위 11..120).",
                            track + 1,
                            pitch
                        );
                        let index = notes.len();
                        notes.push(MidiNote {
                            on: time,
                            off: None,
                            track,
                            port,
                            channel: channel_index,
                            pitch,
                            velocity: vel.as_int(),
                            program: state.program,
                        });
                        state.keys[usize::from(pitch)].push_back(index);
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        state.note_off(track, key.as_int(), time, &mut notes);
                    }
                    MidiMessage::ProgramChange { program } => state.program = program.as_int(),
                    MidiMessage::Controller { controller, value } => match controller.as_int() {
                        64 if value.as_int() >= 64 => state.pedal = true,
                        64 | 121 => state.release_pedal(time, &mut notes),
                        // CC120은 즉시 종료하고 CC123은 건반 해제로 처리해 페달을 적용합니다.
                        120 => state.all_notes_off(time, &mut notes, true),
                        123 => state.all_notes_off(time, &mut notes, false),
                        _ => {}
                    },
                    _ => {}
                }
            }
            TrackEventKind::Meta(MetaMessage::Tempo(value)) => {
                let value = value.as_int();
                ensure!(value > 0, "MIDI 템포가 0입니다 (트랙 {}).", track + 1);
                tempos.insert(time, value);
            }
            TrackEventKind::Meta(MetaMessage::TimeSignature(numerator, power, _, _)) => {
                let denominator = 1_u32
                    .checked_shl(u32::from(power))
                    .context("MIDI 박자표의 분모가 너무 큽니다.")?;
                ensure!(
                    numerator > 0 && denominator <= 384,
                    "MIDI 박자표 {numerator}/{denominator}를 MMI로 표현할 수 없습니다."
                );
                signatures.insert(time, (numerator, denominator));
            }
            TrackEventKind::Meta(MetaMessage::TrackName(value)) if names[track].is_empty() => {
                names[track] = clean_name(value);
            }
            TrackEventKind::Meta(MetaMessage::MidiPort(port)) => {
                track_ports[track] = port.as_int();
            }
            _ => {}
        }
    }
    // 다른 트랙의 페달이 음을 유지할 수 있으므로 미종료 음은 파일 전체 끝에서 닫습니다.
    for state in channels.values_mut() {
        state.all_notes_off(end, &mut notes, true);
    }

    let mut boundaries = BTreeSet::from([0_u64]);
    boundaries.extend(tempos.keys().copied());
    boundaries.extend(signatures.keys().copied());
    for note in &notes {
        boundaries.insert(note.on);
        boundaries.insert(note.off.expect("all keys and pedal voices were closed"));
    }
    let mapped = map_boundaries(&boundaries, &tempos, &mut clock)?;
    let mut target_boundaries: BTreeSet<Tick> = mapped.points.values().copied().collect();
    let mut groups: BTreeMap<(usize, u8, u8), Vec<Note>> = BTreeMap::new();
    let mut programs: BTreeMap<(usize, u8, u8), BTreeSet<u8>> = BTreeMap::new();
    let mut extended = 0_usize;
    for source in &notes {
        let on = mapped.points[&source.on];
        let mut off = mapped.points[&source.off.expect("closed note")];
        if off == on {
            // 최소 종료점을 넣어 양쪽에 6틱을 확보할 수 없으면 다음 기존 경계를 써서 공격을 보존합니다.
            off = minimum_end(on, &target_boundaries)?;
            target_boundaries.insert(off);
            extended += 1;
        }
        let key = (source.track, source.port, source.channel);
        groups.entry(key).or_default().push(Note {
            on,
            off,
            pitch: i32::from(source.pitch),
            vel: ((i32::from(source.velocity) * 15 + 63) / 127).clamp(1, 15),
            src: (0, 0),
        });
        programs.entry(key).or_default().insert(source.program);
    }
    let total = groups
        .values()
        .flatten()
        .map(|note| note.off)
        .max()
        .unwrap_or(0);
    let mut score = Score {
        head: vec![
            "[mml-score]".into(),
            "version=1".into(),
            format!(
                "tempo={}",
                mapped
                    .tempos
                    .iter()
                    .map(|(tick, bpm)| format!("{tick}T{bpm}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            format!("midiTimingAdjustedEvents={}", mapped.adjusted),
            format!("midiExtendedShortNotes={extended}"),
        ],
        ..Score::default()
    };
    for ((source_track, port, channel), mut group) in groups {
        group.sort_by_key(|note| (note.on, note.pitch, note.off));
        let lanes = monophonic_lanes(group)?;
        for (chunk_index, chunk) in lanes.chunks(3).enumerate() {
            let mut parts = Vec::with_capacity(3);
            for part in 0..3 {
                let lane = chunk.get(part).map(Vec::as_slice).unwrap_or(&[]);
                parts.push(
                    emit_part(lane, if part == 0 { &mapped.tempos } else { &[] }, total)
                        .with_context(|| format!("MIDI 트랙 {}의 MML 변환", source_track + 1))?,
                );
            }
            let base_name = if names[source_track].is_empty() {
                format!("MIDI {}", source_track + 1)
            } else {
                names[source_track].clone()
            };
            let suffix = if lanes.len() > 3 {
                format!(" · {}", chunk_index + 1)
            } else {
                String::new()
            };
            // MIDI 프로그램 번호는 MabiIcco 악기 번호와 달라 별도 메타데이터에만 보존합니다.
            let meta = BTreeMap::from([
                (
                    "name".into(),
                    format!("{base_name} · 채널 {}{suffix}", channel + 1),
                ),
                ("program".into(), "0".into()),
                ("songProgram".into(), "-1".into()),
                ("panpot".into(), "64".into()),
                ("visible".into(), "true".into()),
                ("midiSourceTrack".into(), (source_track + 1).to_string()),
                ("midiChannel".into(), (channel + 1).to_string()),
                ("midiPort".into(), port.to_string()),
                (
                    "midiPrograms".into(),
                    programs[&(source_track, port, channel)]
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ]);
            score.tracks.push(track_from_mml(
                format!("MML@{};", parts.join(",")),
                meta,
                score.tracks.len(),
            )?);
        }
    }
    let mut target_signatures = BTreeMap::new();
    for (tick, signature) in signatures {
        target_signatures.insert(mapped.points[&tick], signature);
    }
    score.tail.push("[time-signature]".into());
    score.tail.extend(
        target_signatures
            .into_iter()
            .map(|(tick, (num, den))| format!("{tick}={num}/{den}")),
    );
    ensure!(
        score.all_notes().len() == notes.len(),
        "MIDI 음표 수가 MML 변환 중 변경되었습니다."
    );
    Ok(score)
}

/// 트랙 이름에서 줄바꿈과 제어 문자를 제거합니다.
fn clean_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                character
            }
        })
        .take(160)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 겹치는 MIDI 음표를 음높이 연결을 고려한 단선율 파트로 나눕니다.
fn monophonic_lanes(notes: Vec<Note>) -> Result<Vec<Vec<Note>>> {
    let mut lanes: Vec<Vec<Note>> = Vec::new();
    for note in notes {
        let best = lanes
            .iter()
            .enumerate()
            .filter_map(|(index, lane)| {
                let last = lane.last()?;
                (last.off <= note.on).then_some((
                    index,
                    (last.pitch - note.pitch).abs(),
                    note.on - last.off,
                ))
            })
            .min_by_key(|&(index, distance, gap)| (distance, gap, index))
            .map(|item| item.0);
        if let Some(index) = best {
            lanes[index].push(note);
        } else {
            ensure!(
                lanes.len() < 256,
                "MIDI 트랙·채널의 동시발음이 너무 많습니다 (최대 256파트)."
            );
            lanes.push(vec![note]);
        }
    }
    Ok(lanes)
}

/// 다음 경계와 최소 음가를 함께 만족하는 종료 시점을 구합니다.
fn minimum_end(on: Tick, boundaries: &BTreeSet<Tick>) -> Result<Tick> {
    use std::ops::Bound::{Excluded, Unbounded};
    let minimum = on
        .checked_add(MIN_TICK)
        .context("MIDI 악보 길이가 너무 깁니다.")?;
    Ok(boundaries
        .range((Excluded(on), Unbounded))
        .next()
        .copied()
        .filter(|&next| next < minimum + MIN_TICK)
        .unwrap_or(minimum))
}

// PPQN은 delta_ticks × 4분음표당 마이크로초를 누적하고 SMPTE 드롭 프레임은 30000/1001을 씁니다.
struct SourceClock {
    numerator: u128,
    denominator: u128,
    last_tick: u64,
    micros_per_quarter: u32,
    fixed_tick_numerator: Option<u128>,
}

impl SourceClock {
    /// PPQN 또는 SMPTE의 유리수 시간 누적기를 초기화합니다.
    fn new(timing: Timing) -> Result<Self> {
        let (denominator, fixed_tick_numerator) = match timing {
            Timing::Metrical(ppqn) => {
                ensure!(ppqn.as_int() > 0, "MIDI PPQN이 0입니다.");
                (u128::from(ppqn.as_int()), None)
            }
            Timing::Timecode(fps, subframes) => {
                ensure!(subframes > 0, "MIDI SMPTE의 프레임당 tick 수가 0입니다.");
                let (numerator, frames) = match fps {
                    Fps::Fps24 => (1_000_000, 24),
                    Fps::Fps25 => (1_000_000, 25),
                    Fps::Fps29 => (1_001_000_000, 30_000),
                    Fps::Fps30 => (1_000_000, 30),
                };
                (frames * u128::from(subframes), Some(numerator))
            }
        };
        Ok(Self {
            numerator: 0,
            denominator,
            last_tick: 0,
            micros_per_quarter: 500_000,
            fixed_tick_numerator,
        })
    }

    /// 원본 틱까지 시간을 누적해 절대 초 단위 시점을 반환합니다.
    fn advance(&mut self, tick: u64) -> Result<f64> {
        let delta = tick
            .checked_sub(self.last_tick)
            .context("MIDI 시간이 역순입니다.")?;
        let factor = self
            .fixed_tick_numerator
            .unwrap_or(u128::from(self.micros_per_quarter));
        self.numerator = self
            .numerator
            .checked_add(u128::from(delta) * factor)
            .context("MIDI 시간 범위를 초과했습니다.")?;
        self.last_tick = tick;
        Ok(self.numerator as f64 / self.denominator as f64 / 1_000_000.0)
    }
}

struct MappedBoundaries {
    points: BTreeMap<u64, Tick>,
    tempos: Vec<Tempo>,
    adjusted: usize,
}

/// 원본 절대 시간과 정수 MML 템포의 오차를 보정하며 경계를 배치합니다.
fn map_boundaries(
    boundaries: &BTreeSet<u64>,
    source_tempos: &BTreeMap<u64, u32>,
    clock: &mut SourceClock,
) -> Result<MappedBoundaries> {
    let mut points = BTreeMap::new();
    let mut output_tempos = BTreeMap::from([(0, 120)]);
    let mut target_tick = 0_i64;
    let mut encoded_seconds = 0.0_f64;
    let mut bpm = 120_i32;
    let mut adjusted = 0_usize;
    for &source_tick in boundaries {
        let actual_seconds = clock.advance(source_tick)?;
        let ticks_per_second = 96.0 * f64::from(bpm) / 60.0;
        // 원본 절대 시간과의 차이를 매번 반영해 정수 BPM 및 짧은 간격 보정 오차가 누적되지 않게 합니다.
        let desired = target_tick as f64 + (actual_seconds - encoded_seconds) * ticks_per_second;
        ensure!(
            desired.is_finite() && desired < i32::MAX as f64,
            "MIDI 악보 길이가 MMI의 표현 범위를 초과합니다."
        );
        let rounded = (desired.round() as Tick).max(target_tick);
        let next = if rounded - target_tick >= MIN_TICK {
            rounded
        } else if desired - target_tick as f64 > MIN_TICK as f64 * 0.5 {
            target_tick + MIN_TICK
        } else {
            target_tick
        };
        adjusted += usize::from(next != desired.round() as Tick);
        encoded_seconds += (next - target_tick) as f64 / ticks_per_second;
        target_tick = next;
        points.insert(source_tick, target_tick);
        if let Some(&micros) = source_tempos.get(&source_tick) {
            clock.micros_per_quarter = micros;
            // BPM 제한으로 생긴 차이는 위의 길이 보정에 반영하며 원본 시간을 바꾸지 않습니다.
            bpm = (60_000_000.0 / f64::from(micros)).round().clamp(1.0, 255.0) as i32;
            output_tempos.insert(target_tick, bpm);
        }
    }
    let mut previous = None;
    let tempos = output_tempos
        .into_iter()
        .filter(|&(_, bpm)| {
            let changed = previous != Some(bpm);
            previous = Some(bpm);
            changed
        })
        .collect();
    Ok(MappedBoundaries {
        points,
        tempos,
        adjusted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 짧은 경계 보정이 정상적인 셋잇단음을 격자에 강제로 맞추지 않는지 검사합니다.
    fn local_short_boundaries_do_not_quantize_regular_triplets_to_a_six_tick_grid() {
        let mut clock = SourceClock::new(Timing::Metrical(96.into())).unwrap();
        let points = BTreeSet::from([0, 1, 2, 32, 64, 96]);
        let result = map_boundaries(&points, &BTreeMap::from([(0, 500_000)]), &mut clock).unwrap();
        assert_eq!(
            result.points.values().copied().collect::<Vec<_>>(),
            [0, 0, 0, 32, 64, 96]
        );
        assert_eq!(minimum_end(0, &BTreeSet::from([0, 8, 32])).unwrap(), 8);
        assert_eq!(minimum_end(0, &BTreeSet::from([0, 32])).unwrap(), 6);
    }

    #[test]
    /// SMPTE 드롭 프레임 비율과 템포 독립성을 검사합니다.
    fn source_clock_uses_exact_drop_frame_ratio_and_ignores_musical_tempo_for_smpte() {
        let mut clock = SourceClock::new(Timing::Timecode(Fps::Fps29, 100)).unwrap();
        clock.micros_per_quarter = 1_000_000;
        assert!((clock.advance(3_000_000).unwrap() - 1_001.0).abs() < 1e-10);
    }

    #[test]
    /// 긴 구간에서 소수 템포 보정 오차가 누적되지 않는지 검사합니다.
    fn fractional_tempo_compensation_is_absolute_over_long_sections() {
        let mut clock = SourceClock::new(Timing::Metrical(960.into())).unwrap();
        let points = BTreeSet::from([0, 960, 96_000, 1_000_000, 10_000_000]);
        let tempos = BTreeMap::from([(0, 497_925), (1_000_000, 501_001)]);
        let result = map_boundaries(&points, &tempos, &mut clock).unwrap();
        let mut seconds = 0.0;
        let mut tick = 0;
        let mut bpm = 120;
        for &(at, next) in &result.tempos {
            seconds += (at - tick) as f64 * 60.0 / (96.0 * f64::from(bpm));
            tick = at;
            bpm = next;
        }
        seconds += (result.points[&10_000_000] - tick) as f64 * 60.0 / (96.0 * f64::from(bpm));
        let original = (1_000_000.0 * 497_925.0 + 9_000_000.0 * 501_001.0) / 960.0 / 1_000_000.0;
        assert!((seconds - original).abs() < 0.003);
    }

    #[test]
    /// 트랙 이름으로 MMI 행을 삽입할 수 없는지 검사합니다.
    fn names_cannot_inject_mmi_lines() {
        let name = clean_name(b"Piano\nvisible=false\rmml-track=MML@c;\x00");
        assert!(!name.chars().any(char::is_control));
        assert!(name.contains("Piano visible=false mml-track="));
    }

    #[test]
    /// CC120과 CC123의 서로 다른 페달 처리 규칙을 검사합니다.
    fn all_sound_off_keeps_pedal_controller_while_all_notes_off_honors_it() {
        let mut channel = Channel::default();
        let mut notes = (0..3)
            .map(|index| MidiNote {
                port: 0,
                on: index * 10,
                off: None,
                track: 0,
                channel: 0,
                pitch: 60,
                velocity: 100,
                program: 0,
            })
            .collect::<Vec<_>>();
        channel.pedal = true;
        channel.keys[60].push_back(0);
        channel.all_notes_off(10, &mut notes, false);
        assert_eq!(notes[0].off, None);
        channel.keys[60].push_back(1);
        channel.all_notes_off(20, &mut notes, true);
        assert_eq!(notes[0].off, Some(20));
        assert_eq!(notes[1].off, Some(20));
        assert!(channel.pedal);
        channel.keys[60].push_back(2);
        channel.note_off(0, 60, 30, &mut notes);
        assert_eq!(notes[2].off, None);
        channel.release_pedal(40, &mut notes);
        assert_eq!(notes[2].off, Some(40));
    }
}
