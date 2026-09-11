//! 악보의 템포와 음표를 샘플 시계에 맞춰 합성한다.

use std::collections::BTreeMap;
use std::f64::consts::FRAC_1_SQRT_2;
use std::sync::Arc;

use anyhow::{Result, ensure};

use crate::{
    Score, Tick,
    instruments::{Instrument, PartSynth},
};

#[path = "piano.rs"]
pub(crate) mod piano;

const TICKS_PER_QUARTER: f64 = 96.0;
const RELEASE_SECONDS: f64 = piano::MAX_RELEASE_SECONDS;
const SEEK_FADE_SECONDS: f64 = 0.008;
const GATE_FADE_SECONDS: f64 = 0.008;

#[derive(Clone, Debug)]
pub struct ScheduledNote {
    pub track: usize,
    pub part: usize,
    pub start: f64,
    pub end: f64,
    pub pitch: i32,
    /// MML v0..v15를 0..1로 정규화한 세기다.
    pub velocity: f32,
}

#[derive(Clone, Debug)]
struct TempoSegment {
    tick: f64,
    seconds: f64,
    seconds_per_tick: f64,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub(crate) notes: Vec<ScheduledNote>,
    pub(crate) duration_seconds: f64,
    pub(crate) total_ticks: Tick,
    tempos: Vec<TempoSegment>,
    voice_capacity: usize,
    render_end: f64,
    part_counts: Vec<usize>,
}

impl Timeline {
    /// 재생 순서의 음표를 읽기 전용으로 반환하며, 수정하려면 타임라인을 다시 만들어야 한다.
    pub fn notes(&self) -> &[ScheduledNote] {
        &self.notes
    }

    /// 릴리스 꼬리를 제외한 악보의 재생 길이를 초로 반환한다.
    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    /// 악보의 전체 tick 길이를 반환한다.
    pub fn total_ticks(&self) -> Tick {
        self.total_ticks
    }

    /// 원본의 템포·파트 위치와 음표로 재생 타임라인을 만든다.
    pub fn from_score(score: &Score) -> Result<Self> {
        let total_ticks = score.total_ticks();
        ensure!(total_ticks >= 0, "Score duration cannot be negative");
        let mut events = BTreeMap::from([(0, 120)]);
        // 같은 tick의 템포는 원본 순서에서 마지막 사건을 적용한다.
        for (tick, bpm) in score.tempos() {
            ensure!(tick >= 0 && bpm > 0, "Invalid tempo {bpm} at tick {tick}");
            events.insert(tick, bpm);
        }
        let mut tempos = Vec::with_capacity(events.len());
        let mut seconds = 0.0;
        let mut previous_tick = 0;
        let mut seconds_per_tick = 60.0 / (120.0 * TICKS_PER_QUARTER);
        for (tick, bpm) in events {
            if tick > total_ticks {
                break;
            }
            seconds += (tick - previous_tick) as f64 * seconds_per_tick;
            seconds_per_tick = 60.0 / (f64::from(bpm) * TICKS_PER_QUARTER);
            tempos.push(TempoSegment {
                tick: tick as f64,
                seconds,
                seconds_per_tick,
            });
            previous_tick = tick;
        }
        let duration_seconds = seconds + (total_ticks - previous_tick) as f64 * seconds_per_tick;
        let mut timeline = Self {
            notes: Vec::new(),
            duration_seconds,
            total_ticks,
            tempos,
            voice_capacity: 0,
            render_end: duration_seconds,
            part_counts: score.tracks.iter().map(|track| track.parts.len()).collect(),
        };
        for (ti, track) in score.tracks.iter().enumerate() {
            for (pi, note) in track
                .parts
                .iter()
                .enumerate()
                .flat_map(|(pi, part)| part.iter().map(move |note| (pi, note)))
            {
                ensure!(
                    note.on >= 0 && note.off >= note.on,
                    "Invalid note interval {}..{}",
                    note.on,
                    note.off
                );
                if note.on == note.off {
                    continue;
                }
                timeline.notes.push(ScheduledNote {
                    track: ti,
                    part: pi,
                    start: timeline.tick_to_seconds(note.on as f64),
                    end: timeline.tick_to_seconds(note.off as f64),
                    pitch: note.pitch,
                    velocity: note.vel.clamp(0, 15) as f32 / 15.0,
                });
            }
        }
        timeline.notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        // 재생 중 할당하지 않도록 겹치는 음과 릴리스 꼬리의 최대 용량을 미리 확보한다.
        let mut voice_events = Vec::with_capacity(timeline.notes.len() * 2);
        for note in &timeline.notes {
            if note.velocity > 0.0 {
                voice_events.push((note.start, 1_i32));
                voice_events.push((note.end + RELEASE_SECONDS, -1_i32));
                timeline.render_end = timeline.render_end.max(note.end + RELEASE_SECONDS);
            }
        }
        voice_events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut active = 0;
        for (_, change) in voice_events {
            active += change;
            timeline.voice_capacity = timeline.voice_capacity.max(active.max(0) as usize);
        }
        Ok(timeline)
    }

    /// 악보 경계 안의 tick을 템포 변화에 맞춰 초로 변환한다.
    pub fn tick_to_seconds(&self, tick: f64) -> f64 {
        let tick = bounded(tick, self.total_ticks as f64);
        let index = self.tempos.partition_point(|event| event.tick <= tick) - 1;
        let event = &self.tempos[index];
        event.seconds + (tick - event.tick) * event.seconds_per_tick
    }

    /// 지속음 도중의 템포 변화도 반영해 초를 악보 경계 안의 tick으로 변환한다.
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let seconds = bounded(seconds, self.duration_seconds);
        let index = self
            .tempos
            .partition_point(|event| event.seconds <= seconds)
            - 1;
        let event = &self.tempos[index];
        (event.tick + (seconds - event.seconds) / event.seconds_per_tick)
            .clamp(0.0, self.total_ticks as f64)
    }
}

/// 트랙·파트 음소거와 악기 선택을 서로 독립적으로 저장한다.
#[derive(Clone, Debug, Default)]
pub(crate) struct Audibility {
    tracks: Vec<bool>,
    parts: Vec<Vec<bool>>,
    instruments: Vec<Vec<Instrument>>,
}

impl Audibility {
    /// 모든 트랙·파트를 켜고 기본 악기를 선택한다.
    pub(crate) fn all(timeline: &Timeline) -> Self {
        Self {
            tracks: vec![true; timeline.part_counts.len()],
            parts: timeline
                .part_counts
                .iter()
                .map(|&n| vec![true; n])
                .collect(),
            instruments: timeline
                .part_counts
                .iter()
                .map(|&n| vec![Instrument::default(); n])
                .collect(),
        }
    }

    /// 트랙의 소리 선택 상태를 반환한다.
    pub(crate) fn track_enabled(&self, track: usize) -> bool {
        self.tracks.get(track).copied().unwrap_or(false)
    }

    /// 트랙 음소거와 독립적인 파트 자체의 선택 상태를 반환한다.
    pub(crate) fn part_enabled(&self, track: usize, part: usize) -> bool {
        self.parts
            .get(track)
            .and_then(|parts| parts.get(part))
            .copied()
            .unwrap_or(false)
    }

    /// 트랙과 파트가 모두 켜져 있는지 확인한다.
    fn enabled(&self, track: usize, part: usize) -> bool {
        self.track_enabled(track) && self.part_enabled(track, part)
    }

    /// 파트의 선택 악기를 반환한다.
    pub(crate) fn part_instrument(&self, track: usize, part: usize) -> Instrument {
        self.instruments
            .get(track)
            .and_then(|parts| parts.get(part))
            .copied()
            .unwrap_or_default()
    }

    /// 파트의 악기 선택을 저장하고 실제 변경 여부를 반환한다.
    pub(crate) fn set_part_instrument(
        &mut self,
        track: usize,
        part: usize,
        instrument: Instrument,
    ) -> bool {
        match self
            .instruments
            .get_mut(track)
            .and_then(|parts| parts.get_mut(part))
        {
            Some(value) if *value != instrument => {
                *value = instrument;
                true
            }
            _ => false,
        }
    }

    /// 파트별 선택을 유지하며 트랙의 소리 상태를 바꾼다.
    pub(crate) fn set_track_enabled(&mut self, track: usize, enabled: bool) -> bool {
        match self.tracks.get_mut(track) {
            Some(value) if *value != enabled => {
                *value = enabled;
                true
            }
            _ => false,
        }
    }

    /// 트랙 상태와 독립적으로 파트의 소리 상태를 바꾼다.
    pub(crate) fn set_part_enabled(&mut self, track: usize, part: usize, enabled: bool) -> bool {
        match self
            .parts
            .get_mut(track)
            .and_then(|parts| parts.get_mut(part))
        {
            Some(value) if *value != enabled => {
                *value = enabled;
                true
            }
            _ => false,
        }
    }
}

/// NaN을 0으로 바꾸고 값을 유효 범위 안으로 제한한다.
fn bounded(value: f64, maximum: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, maximum)
    }
}

struct Voice {
    tone: piano::Tone,
    start: f64,
    end: f64,
    gain: f64,
    track: usize,
    part: usize,
    gate_gain: f64,
}

impl Voice {
    /// 음표의 피아노 녹음과 시작 시 소리 상태를 준비한다.
    fn new(note: &ScheduledNote, sample_rate: f64, enabled: bool) -> Option<Self> {
        let frequency = 440.0 * ((f64::from(note.pitch) - 69.0) / 12.0).exp2();
        if note.velocity <= 0.0
            || !frequency.is_finite()
            || frequency < 8.0
            || frequency >= sample_rate * 0.45
        {
            return None;
        }
        let velocity = f64::from(note.velocity.clamp(0.0, 1.0));
        // 기존 중앙 음량을 유지하며 모든 음을 모노로 합친다.
        let gain = 0.35 * FRAC_1_SQRT_2 * velocity * velocity;
        let tone = piano::Tone::new(note.pitch, (velocity * 127.0).round() as i32)?;
        Some(Self {
            tone,
            start: note.start,
            end: note.end,
            gain,
            track: note.track,
            part: note.part,
            gate_gain: f64::from(enabled),
        })
    }

    /// 음표의 경과시간과 음소거 페이드를 반영해 한 샘플을 계산한다.
    fn sample(&mut self, now: f64, enabled: bool, gate_step: f64) -> f64 {
        let target = f64::from(enabled);
        self.gate_gain += (target - self.gate_gain).clamp(-gate_step, gate_step);
        if self.gate_gain == 0.0 {
            return 0.0;
        }
        let sample = self.tone.sample(now - self.start, self.end - self.start);
        sample * self.gain * self.gate_gain
    }
}

pub struct Renderer {
    timeline: Arc<Timeline>,
    sample_rate: f64,
    base_seconds: f64,
    frames: u64,
    next_note: usize,
    voices: Vec<Voice>,
    fade_frames: u64,
    audibility: Audibility,
    gate_step: f64,
    instrument_parts: Vec<Vec<InstrumentPart>>,
    pending_offs: Vec<PendingOff>,
    instrument_render_end: f64,
}

struct InstrumentPart {
    synth: PartSynth,
    gate_gain: f64,
    render_until: f64,
}

struct PendingOff {
    end: f64,
    track: usize,
    part: usize,
    pitch: i32,
}

impl Renderer {
    /// 모든 파트가 켜진 샘플 시계 렌더러를 만든다.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(timeline: Arc<Timeline>, sample_rate: u32) -> Self {
        Self::try_new(timeline, sample_rate)
            .expect("supported sample rate and verified embedded instrument banks")
    }

    /// 음원이 준비되었는지 확인하고 모든 파트가 켜진 렌더러를 만든다.
    pub fn try_new(timeline: Arc<Timeline>, sample_rate: u32) -> Result<Self> {
        let audibility = Audibility::all(&timeline);
        Self::try_with_audibility(timeline, sample_rate, audibility)
    }

    /// 선택 상태에 맞춰 음원과 파트별 렌더러를 미리 준비한다.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn with_audibility(
        timeline: Arc<Timeline>,
        sample_rate: u32,
        audibility: Audibility,
    ) -> Self {
        Self::try_with_audibility(timeline, sample_rate, audibility)
            .expect("supported sample rate and verified embedded instrument banks")
    }

    /// 선택 상태와 음원 준비 여부를 확인하며 파트별 렌더러를 만든다.
    pub(crate) fn try_with_audibility(
        timeline: Arc<Timeline>,
        sample_rate: u32,
        audibility: Audibility,
    ) -> Result<Self> {
        let sample_rate = sample_rate.clamp(16_000, 192_000);
        piano::load()?;
        let instrument_parts = timeline
            .part_counts
            .iter()
            .enumerate()
            .map(|(ti, &count)| {
                (0..count)
                    .map(|pi| {
                        let mut synth = PartSynth::new(sample_rate)?;
                        synth.set_instrument(audibility.part_instrument(ti, pi));
                        Ok(InstrumentPart {
                            synth,
                            gate_gain: f64::from(audibility.enabled(ti, pi)),
                            render_until: 0.0,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let sample_rate = f64::from(sample_rate.max(1));
        Ok(Self {
            voices: Vec::with_capacity(timeline.voice_capacity),
            pending_offs: Vec::with_capacity(timeline.voice_capacity),
            instrument_parts,
            instrument_render_end: 0.0,
            timeline,
            sample_rate,
            base_seconds: 0.0,
            frames: 0,
            next_note: 0,
            fade_frames: (sample_rate * SEEK_FADE_SECONDS).ceil() as u64,
            audibility,
            gate_step: 1.0 / (sample_rate * GATE_FADE_SECONDS).max(1.0),
        })
    }

    /// 트랙의 소리 선택 상태를 반환한다.
    pub fn track_enabled(&self, track: usize) -> bool {
        self.audibility.track_enabled(track)
    }

    /// 트랙 음소거와 독립적인 파트 자체의 선택 상태를 반환한다.
    pub fn part_enabled(&self, track: usize, part: usize) -> bool {
        self.audibility.part_enabled(track, part)
    }

    /// 파트별 선택을 유지하며 트랙의 소리 상태를 바꾼다.
    pub fn set_track_enabled(&mut self, track: usize, enabled: bool) {
        self.audibility.set_track_enabled(track, enabled);
    }

    /// 트랙 상태와 독립적으로 파트의 소리 상태를 바꾼다.
    pub fn set_part_enabled(&mut self, track: usize, part: usize, enabled: bool) {
        self.audibility.set_part_enabled(track, part, enabled);
    }

    /// 파트의 선택 악기를 반환한다.
    pub fn part_instrument(&self, track: usize, part: usize) -> Instrument {
        self.audibility.part_instrument(track, part)
    }

    /// 파트의 악기를 바꾸되 이미 울리는 음은 원래 음색으로 끝낸다.
    pub fn set_part_instrument(&mut self, track: usize, part: usize, instrument: Instrument) {
        if self.audibility.set_part_instrument(track, part, instrument) {
            self.instrument_parts[track][part]
                .synth
                .set_instrument(instrument);
        }
    }

    /// 무음 상태에서 선택 변경을 즉시 반영해 재개 직후의 소리 누출을 막는다.
    pub(crate) fn sync_audibility(&mut self) {
        for voice in &mut self.voices {
            voice.gate_gain = f64::from(self.audibility.enabled(voice.track, voice.part));
        }
        for (ti, parts) in self.instrument_parts.iter_mut().enumerate() {
            for (pi, part) in parts.iter_mut().enumerate() {
                part.gate_gain = f64::from(self.audibility.enabled(ti, pi));
            }
        }
    }

    /// 탐색 기준점과 출력 샘플 수로 현재 시각을 계산한다.
    fn now(&self) -> f64 {
        self.base_seconds + self.frames as f64 / self.sample_rate
    }

    /// 원래 경과시간의 지속음과 릴리스를 복원하며 피아노는 샘플 위치, SF2는 사건 재생을 사용한다.
    pub fn seek(&mut self, seconds: f64) {
        self.base_seconds = bounded(seconds, self.timeline.duration_seconds);
        self.frames = 0;
        self.voices.clear();
        self.pending_offs.clear();
        self.instrument_render_end = 0.0;
        self.next_note = self
            .timeline
            .notes
            .partition_point(|note| note.start <= self.base_seconds);
        for note in &self.timeline.notes[..self.next_note] {
            if self.audibility.part_instrument(note.track, note.part) == Instrument::Piano
                && note.end + RELEASE_SECONDS > self.base_seconds
                && let Some(voice) = Voice::new(
                    note,
                    self.sample_rate,
                    self.audibility.enabled(note.track, note.part),
                )
            {
                self.voices.push(voice);
            }
        }
        // SF2 상태는 원래 타격부터 복원하므로 Player가 콜백 밖에서 탐색 렌더러를 준비한다.
        for (ti, parts) in self.instrument_parts.iter_mut().enumerate() {
            for (pi, part) in parts.iter_mut().enumerate() {
                part.synth.reset();
                part.render_until = 0.0;
                part.gate_gain = f64::from(self.audibility.enabled(ti, pi));
                if self.audibility.part_instrument(ti, pi) == Instrument::Piano {
                    continue;
                }
                let release = self.audibility.part_instrument(ti, pi).release_seconds();
                let mut events = Vec::new();
                for note in &self.timeline.notes[..self.next_note] {
                    if note.track != ti
                        || note.part != pi
                        || note.velocity <= 0.0
                        || note.end + release <= self.base_seconds
                    {
                        continue;
                    }
                    events.push((note.start, true, note.pitch, midi_velocity(note.velocity)));
                    part.render_until = part.render_until.max(note.end + release);
                    self.instrument_render_end = self.instrument_render_end.max(note.end + release);
                    if note.end <= self.base_seconds {
                        events.push((note.end, false, note.pitch, 0));
                    } else {
                        self.pending_offs.push(PendingOff {
                            end: note.end,
                            track: ti,
                            part: pi,
                            pitch: note.pitch,
                        });
                    }
                }
                events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                let Some(first) = events.first() else {
                    continue;
                };
                let origin = first.0;
                let mut frame = 0_u64;
                for (time, attack, pitch, velocity) in events {
                    let at = ((time - origin) * self.sample_rate).round() as u64;
                    part.synth.render_discard(at.saturating_sub(frame));
                    frame = at;
                    if attack {
                        part.synth.note_on(pitch, velocity);
                    } else {
                        part.synth.note_off(pitch);
                    }
                }
                let target = ((self.base_seconds - origin) * self.sample_rate).round() as u64;
                part.synth.render_discard(target.saturating_sub(frame));
            }
        }
    }

    /// 릴리스 꼬리가 악보 끝을 넘지 않도록 현재 재생 위치를 반환한다.
    pub fn position_seconds(&self) -> f64 {
        self.now().min(self.timeline.duration_seconds)
    }

    /// 릴리스 꼬리까지 모두 재생했는지 확인한다.
    pub fn finished(&self) -> bool {
        self.now() >= self.timeline.render_end.max(self.instrument_render_end)
    }

    /// 샘플 시계에 맞춰 음표를 합성하고 같은 모노 출력을 좌우에 반환한다.
    pub fn next_frame(&mut self) -> [f32; 2] {
        if self.finished() {
            return [0.0; 2];
        }
        let now = self.now();
        // 같은 샘플 시점에서는 끝난 꼬리를 먼저 회수한 뒤 새 음을 추가한다.
        self.voices
            .retain(|voice| now < voice.end + RELEASE_SECONDS);
        self.pending_offs.retain(|off| {
            if off.end <= now {
                self.instrument_parts[off.track][off.part]
                    .synth
                    .note_off(off.pitch);
                false
            } else {
                true
            }
        });
        while let Some(note) = self.timeline.notes.get(self.next_note) {
            if note.start > now {
                break;
            }
            if self.audibility.part_instrument(note.track, note.part) != Instrument::Piano {
                if note.velocity > 0.0 && note.end > now {
                    let release = self
                        .audibility
                        .part_instrument(note.track, note.part)
                        .release_seconds();
                    self.instrument_parts[note.track][note.part]
                        .synth
                        .note_on(note.pitch, midi_velocity(note.velocity));
                    self.instrument_parts[note.track][note.part].render_until = self
                        .instrument_parts[note.track][note.part]
                        .render_until
                        .max(note.end + release);
                    self.pending_offs.push(PendingOff {
                        end: note.end,
                        track: note.track,
                        part: note.part,
                        pitch: note.pitch,
                    });
                    self.instrument_render_end = self.instrument_render_end.max(note.end + release);
                }
            } else if note.end + RELEASE_SECONDS > now
                && let Some(voice) = Voice::new(
                    note,
                    self.sample_rate,
                    self.audibility.enabled(note.track, note.part),
                )
            {
                self.voices.push(voice);
            }
            self.next_note += 1;
        }
        let mut mixed = 0.0_f64;
        for voice in &mut self.voices {
            mixed += voice.sample(
                now,
                self.audibility.enabled(voice.track, voice.part),
                self.gate_step,
            );
        }
        for (ti, parts) in self.instrument_parts.iter_mut().enumerate() {
            for (pi, part) in parts.iter_mut().enumerate() {
                let target = f64::from(self.audibility.enabled(ti, pi));
                part.gate_gain += (target - part.gate_gain).clamp(-self.gate_step, self.gate_step);
                if now < part.render_until {
                    mixed += part.synth.next_sample() * part.gate_gain;
                } else if part.render_until > 0.0 {
                    part.synth.reset();
                    part.render_until = 0.0;
                }
            }
        }
        let fade = if self.frames < self.fade_frames {
            self.frames as f64 / self.fade_frames as f64
        } else {
            1.0
        };
        self.frames += 1;
        // 정상 음량의 저음은 왜곡하지 않고, 과부하 구간에서만 출력을 제한한다.
        let peak = mixed.abs();
        let gain = if peak > 0.95 {
            fade * (0.95 + 0.05 * ((peak - 0.95) / 0.05).tanh()) / peak
        } else {
            fade
        };
        let sample = mixed * gain;
        let mono = if sample.is_finite() {
            sample.clamp(-1.0, 1.0) as f32
        } else {
            0.0
        };
        [mono; 2]
    }
}

/// 정규화된 MML 세기를 MIDI 세기로 변환한다.
fn midi_velocity(velocity: f32) -> i32 {
    (velocity.clamp(0.0, 1.0) * 127.0).round() as i32
}
