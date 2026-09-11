//! 동시발음 수를 줄이고 음표를 지정한 트랙과 파트에 배치합니다.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};

use crate::core::{MIN_TICK, WHOLE, emit_part, track_from_mml};
use crate::{Note, Score, Tick};

#[cfg(test)]
mod anchor_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub enum Layout {
    Voices,
    Hands,
    Roles,
    Learned,
}

impl Layout {
    /// 배치 방식의 식별 문자열을 반환합니다.
    fn name(self) -> &'static str {
        match self {
            Self::Voices => "voices",
            Self::Hands => "hands",
            Self::Roles => "roles",
            Self::Learned => "learned",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub enum Gain {
    None,
    Auto,
    Shift(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub struct VolumeRange {
    pub min: i32,
    pub max: i32,
}

impl VolumeRange {
    /// 양수 음량 v1~v15를 설정 범위로 반올림해 압축하고 v0는 유지합니다.
    fn compress(self, volume: i32) -> i32 {
        if volume == 0 {
            0
        } else {
            self.min + ((volume - 1) * (self.max - self.min) + 7) / 14
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub struct FoldOptions {
    pub tracks: usize,
    pub parts: usize,
    pub layout: Layout,
    pub gain: Gain,
    pub volume_range: Option<VolumeRange>,
    pub lead_pitch: f64,
    pub lead_continuity: f64,
}

impl Default for FoldOptions {
    /// 기본 편곡 설정을 만듭니다.
    fn default() -> Self {
        Self {
            tracks: 2,
            parts: 3,
            layout: Layout::Hands,
            gain: Gain::Auto,
            volume_range: None,
            lead_pitch: 0.6,
            lead_continuity: 0.7,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldStats {
    pub total: usize,
    pub kept: usize,
    pub truncated: usize,
    pub dropped: usize,
    pub spill: usize,
    pub volume_shift: i32,
}

#[derive(Clone, Debug)]
pub struct FoldResult {
    pub score: Score,
    pub report: String,
    pub stats: FoldStats,
}

struct WorkingNote {
    note: Note,
    activity: f64,
    onset_activity: f64,
    original_velocity: i32,
    base: f64,
}

struct Homes {
    home: Vec<Option<usize>>,
    centre: Vec<f64>,
    roles: Option<Vec<&'static str>>,
    // 명시적으로 추출한 주선율만 고정하며 단순 음역 후보는 고정하지 않습니다.
    primary_lead: Option<Vec<usize>>,
}

/// 원본 음표의 음역·활동·연결 정보를 분석합니다.
fn analyze(score: &Score) -> Result<Vec<WorkingNote>> {
    let mut notes = Vec::new();
    for (ti, track) in score.tracks.iter().enumerate() {
        for (pi, part) in track.parts.iter().enumerate() {
            let mut j = 0;
            let mut k = 0;
            let mut previous_end = 0;
            for (i, note) in part.iter().enumerate() {
                ensure!(
                    note.on >= previous_end && note.off > note.on,
                    "Track {} part {} has overlapping, unsorted, or invalid notes at tick {}",
                    ti + 1,
                    pi + 1,
                    note.on
                );
                ensure!(
                    note.duration() >= MIN_TICK,
                    "Track {} part {} contains a note shorter than the minimum {} ticks at tick {}",
                    ti + 1,
                    pi + 1,
                    MIN_TICK,
                    note.on
                );
                ensure!(
                    (0..=15).contains(&note.vel),
                    "Track {} part {} has volume {} outside 0..=15",
                    ti + 1,
                    pi + 1,
                    note.vel
                );
                previous_end = note.off;
                let activity = if i > 0 && part[i - 1].pitch != note.pitch {
                    0.5
                } else {
                    0.0
                } + if i + 1 < part.len() && part[i + 1].pitch != note.pitch {
                    0.5
                } else {
                    0.0
                };
                while j < part.len() && part[j].off <= note.on.saturating_sub(96) {
                    j += 1;
                }
                while k < part.len() && part[k].on < note.on.saturating_add(96) {
                    k += 1;
                }
                let distinct = part[j..k]
                    .iter()
                    .map(|n| n.pitch)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                let base = 3.0 * f64::from(note.vel)
                    + 60.0 * activity
                    + 8.0 * distinct.min(6) as f64
                    + note.duration().min(384) as f64 / 8.0;
                notes.push(WorkingNote {
                    note: note.clone(),
                    activity,
                    onset_activity: if i > 0
                        && part[i - 1].pitch != note.pitch
                        && note.on - part[i - 1].off <= 96
                    {
                        0.5
                    } else {
                        0.0
                    },
                    original_velocity: note.vel,
                    base,
                });
            }
        }
    }
    Ok(notes)
}

/// 설정한 동시발음 수에 맞춰 공격을 선택하고 겹치는 음의 끝을 줄입니다.
fn reduce(
    notes: &mut [WorkingNote],
    limit: usize,
    attack_priority: bool,
) -> (Vec<usize>, FoldStats) {
    let mut events: Vec<_> = (0..notes.len()).collect();
    events.sort_by_key(|&i| notes[i].note.on);
    let mut active = Vec::new();
    let original_ends: Vec<_> = notes.iter().map(|n| n.note.off).collect();
    let mut sounding = Vec::new();
    let mut next = 0;
    let mut stats = FoldStats {
        total: notes.len(),
        ..FoldStats::default()
    };
    while next < events.len() {
        let tick = notes[events[next]].note.on;
        active.retain(|&i: &usize| notes[i].note.off > tick);
        sounding.retain(|&i: &usize| original_ends[i] > tick);
        while next < events.len() && notes[events[next]].note.on == tick {
            active.push(events[next]);
            sounding.push(events[next]);
            next += 1;
        }
        // 이미 버린 음으로 화음 해석이 바뀌지 않도록 원본의 동시음을 읽습니다.
        let harmony = (active.len() > limit)
            .then(|| crate::harmony::analyze(sounding.iter().map(|&i| notes[i].note.pitch)))
            .flatten();
        while active.len() > limit {
            let mut seen = BTreeMap::<i32, usize>::new();
            let mut pitch_classes = [0usize; 12];
            for &i in &active {
                *seen.entry(notes[i].note.pitch).or_default() += 1;
                pitch_classes[notes[i].note.pitch.rem_euclid(12) as usize] += 1;
            }
            let lo = *seen.first_key_value().expect("active notes").0;
            let hi = *seen.last_key_value().expect("active notes").0;
            let mut victim = 0;
            let mut best_tier = 1;
            let mut best_score = f64::INFINITY;
            for (position, &i) in active.iter().enumerate() {
                let n = &notes[i];
                let p = n.note.pitch;
                let mut value = n.base;
                if p == hi {
                    value += 110.0;
                }
                if p == lo {
                    value += 95.0;
                }
                if pitch_classes[p.rem_euclid(12) as usize] == 1
                    && let Some(harmony) = harmony
                {
                    value += 80.0 * harmony.weight(p);
                }
                let age = tick - n.note.on;
                // 여기서 제거한 공격은 이후 배정에서 복구할 수 없으므로 페달 꼬리의 유지 점수도 감쇠합니다.
                if attack_priority {
                    value *= 0.10 + 0.90 * 2.0_f64.powf(-age as f64 / 48.0);
                }
                if seen[&p] > 1 {
                    value -= 260.0;
                }
                let doubled_octave = p.checked_sub(12).is_some_and(|p| seen.contains_key(&p))
                    || p.checked_add(12).is_some_and(|p| seen.contains_key(&p));
                if p != hi && p != lo && doubled_octave {
                    value -= 70.0;
                }
                // 96틱 이상 울린 음은 더 어린 공격보다 먼저 양보하고, 같은 부류는 중요도로 비교합니다.
                let tier = usize::from(!attack_priority || age < 96);
                if tier < best_tier || (tier == best_tier && value < best_score) {
                    best_tier = tier;
                    best_score = value;
                    victim = position;
                }
            }
            let n = &mut notes[active.remove(victim)].note;
            if tick - n.on >= MIN_TICK {
                n.off = tick;
                stats.truncated += 1;
            } else {
                n.off = n.on;
                stats.dropped += 1;
            }
        }
    }
    let kept: Vec<_> = (0..notes.len())
        .filter(|&i| notes[i].note.duration() >= MIN_TICK)
        .collect();
    stats.kept = kept.len();
    (kept, stats)
}

/// 선택한 음표의 평균 음높이를 계산합니다.
fn mean(notes: &[WorkingNote], ids: &[usize]) -> f64 {
    if ids.is_empty() {
        60.0
    } else {
        ids.iter()
            .map(|&i| f64::from(notes[i].note.pitch))
            .sum::<f64>()
            / ids.len() as f64
    }
}

/// 두 음표 집합의 시간 겹침을 계산합니다.
fn overlap(notes: &[WorkingNote], a: &[usize], b: &[usize]) -> f64 {
    let (mut i, mut j) = (0, 0);
    let mut total = 0.0;
    while i < a.len() && j < b.len() {
        let an = &notes[a[i]].note;
        let bn = &notes[b[j]].note;
        let lo = an.on.max(bn.on);
        let hi = an.off.min(bn.off);
        if hi > lo {
            total += (hi - lo) as f64;
        }
        if an.off < bn.off {
            i += 1;
        } else {
            j += 1;
        }
    }
    total
}

/// 비용이 가장 낮은 첫 후보를 고릅니다.
fn nearest<F: Fn(usize) -> f64>(candidates: impl Iterator<Item = usize>, cost: F) -> usize {
    // 동률이면 원본 순서의 첫 후보를 유지합니다.
    let mut best = None;
    for slot in candidates {
        let value = cost(slot);
        if best.is_none_or(|(_, old)| value < old) {
            best = Some((slot, value));
        }
    }
    best.expect("at least one destination slot").0
}

/// 원본 성부의 음역과 겹침을 기준으로 파트 목적지를 정합니다.
fn voice_homes(notes: &[WorkingNote], kept: &[usize], slots: usize) -> Homes {
    let mut voices: Vec<Vec<usize>> = Vec::new();
    let mut by_source = BTreeMap::new();
    for &i in kept {
        let next = voices.len();
        let v = *by_source.entry(notes[i].note.src).or_insert(next);
        if v == voices.len() {
            voices.push(Vec::new());
        }
        voices[v].push(i);
    }
    for voice in &mut voices {
        voice.sort_by_key(|&i| notes[i].note.on);
    }
    let weight: Vec<f64> = voices
        .iter()
        .map(|voice| voice.iter().map(|&i| notes[i].note.duration() as f64).sum())
        .collect();
    let means: Vec<_> = voices.iter().map(|voice| mean(notes, voice)).collect();
    let mut order: Vec<_> = (0..voices.len()).collect();
    order.sort_by(|&a, &b| weight[b].total_cmp(&weight[a]));
    let mut primary = order[..slots.min(order.len())].to_vec();
    primary.sort_by(|&a, &b| means[b].total_cmp(&means[a]));
    let mut homes = Homes {
        home: vec![None; notes.len()],
        centre: vec![60.0; slots],
        roles: None,
        primary_lead: None,
    };
    let mut content = vec![Vec::new(); slots];
    for (slot, &v) in primary.iter().enumerate() {
        for &i in &voices[v] {
            homes.home[i] = Some(slot);
        }
        content[slot] = voices[v].clone();
        homes.centre[slot] = means[v];
    }
    for &v in order.iter().skip(slots) {
        let span = weight[v].max(1.0);
        let slot = nearest(0..slots, |slot| {
            (means[v] - homes.centre[slot]).abs()
                + 30.0 * overlap(notes, &voices[v], &content[slot]) / span
        });
        for &i in &voices[v] {
            homes.home[i] = Some(slot);
        }
        content[slot].extend_from_slice(&voices[v]);
        content[slot].sort_by_key(|&i| notes[i].note.on);
        homes.centre[slot] = mean(notes, &content[slot]);
    }
    homes
}

/// 음가를 정확히 표현할 수 있도록 전체 음표 구간으로 핵심 성부를 선택합니다.
fn learned_homes(
    notes: &[WorkingNote],
    kept: &[usize],
    probabilities: &[f64],
    options: &FoldOptions,
) -> Result<Homes> {
    let count = options.tracks * options.parts;
    let mut homes = Homes {
        home: vec![None; notes.len()],
        centre: vec![60.0; count],
        roles: None,
        primary_lead: None,
    };
    let mut remaining = kept.to_vec();
    let mut primary = vec![false; notes.len()];
    extract_line(notes, kept, LineKind::Lead, &mut primary, options);
    for track in 0..options.tracks {
        let selected = if track + 1 == options.tracks {
            vec![true; remaining.len()]
        } else {
            let intervals: Vec<_> = remaining.iter().map(|&i| notes[i].note.clone()).collect();
            let weights: Vec<_> = remaining.iter().map(|&i| probabilities[i]).collect();
            let priority: Vec<_> = remaining
                .iter()
                .map(|&i| track == 0 && primary[i])
                .collect();
            crate::core_selection::select(
                &intervals,
                &weights,
                &priority,
                options.parts,
                (options.tracks - track) * options.parts,
            )?
        };
        let group: Vec<_> = remaining
            .iter()
            .zip(&selected)
            .filter_map(|(&i, &core)| core.then_some(i))
            .collect();
        remaining = remaining
            .iter()
            .zip(&selected)
            .filter_map(|(&i, &core)| (!core).then_some(i))
            .collect();
        let local = voice_homes(notes, &group, options.parts);
        let offset = track * options.parts;
        homes.centre[offset..offset + options.parts].copy_from_slice(&local.centre);
        for &i in &group {
            homes.home[i] = local.home[i].map(|part| offset + part);
        }
    }
    Ok(homes)
}

/// 원래 성부 안의 선율 연결을 비교해 각 공격 시점의 주선율 후보를 표시합니다.
fn attack_anchors(
    notes: &[WorkingNote],
    originals: &[Note],
    kept: &[usize],
    options: &FoldOptions,
) -> Vec<bool> {
    let mut events: BTreeMap<Tick, Vec<usize>> = BTreeMap::new();
    for &i in kept {
        events.entry(notes[i].note.on).or_default().push(i);
    }
    let mut anchors = vec![false; notes.len()];
    let mut previous = BTreeMap::<_, &Note>::new();
    let continuity: Vec<_> = originals
        .iter()
        .map(|note| {
            let prior = previous.insert(note.src, note);
            prior
                .filter(|prior| (0..=96).contains(&(note.on - prior.off)))
                .map_or(0.0, |prior| f64::from((note.pitch - prior.pitch).abs()))
        })
        .collect();
    for candidates in events.values() {
        let value = |i: usize| {
            let n = &notes[i];
            3.0 * f64::from(n.original_velocity)
                + 40.0 * n.onset_activity
                + options.lead_pitch * f64::from(n.note.pitch)
                - options.lead_continuity * continuity[i]
        };
        let best = *candidates
            .iter()
            .min_by(|&&a, &&b| value(b).total_cmp(&value(a)).then(a.cmp(&b)))
            .expect("nonempty onset");
        anchors[best] = true;
    }
    anchors
}

/// 실제 파트 배치에서 음표별 목적지와 중심 음역을 복원합니다.
fn slot_homes(notes: &[WorkingNote], slots: &[Vec<usize>]) -> Homes {
    let mut home = vec![None; notes.len()];
    for (slot, ids) in slots.iter().enumerate() {
        for &i in ids {
            home[i] = Some(slot);
        }
    }
    Homes {
        home,
        centre: slots.iter().map(|ids| mean(notes, ids)).collect(),
        roles: None,
        primary_lead: None,
    }
}

/// 선택된 트랙 안에서 성부 연결과 정확한 음가를 유지하며 재배치합니다.
fn repack_tracks(
    notes: &[WorkingNote],
    kept: &[usize],
    homes: &Homes,
    parts: usize,
    legacy: bool,
) -> Result<(Vec<Vec<usize>>, usize)> {
    let mut slots = Vec::with_capacity(homes.centre.len());
    let mut spill = 0;
    for (track, centres) in homes.centre.chunks(parts).enumerate() {
        let offset = track * parts;
        let group: Vec<_> = kept
            .iter()
            .copied()
            .filter(|&i| homes.home[i].is_some_and(|slot| slot / parts == track))
            .collect();
        let local = Homes {
            home: homes
                .home
                .iter()
                .map(|home| home.and_then(|slot| (slot / parts == track).then(|| slot - offset)))
                .collect(),
            centre: centres.to_vec(),
            roles: None,
            primary_lead: None,
        };
        let (output, moved) = if legacy {
            legacy_repack(notes, &group, &local)?
        } else {
            repack(notes, &group, &local)?
        };
        slots.extend(output);
        spill += moved;
    }
    Ok((slots, spill))
}

#[derive(Clone, Copy)]
enum LineKind {
    Lead,
    Bass,
    Mid,
}

/// 이전 음과의 연결 및 음역 선호도를 따라 한 성부를 추출합니다.
fn extract_line(
    notes: &[WorkingNote],
    pool: &[usize],
    kind: LineKind,
    taken: &mut [bool],
    options: &FoldOptions,
) -> Vec<usize> {
    let mut events: BTreeMap<Tick, Vec<usize>> = BTreeMap::new();
    for &i in pool {
        if !taken[i] {
            events.entry(notes[i].note.on).or_default().push(i);
        }
    }
    let mut line = Vec::new();
    let mut end = -1;
    let mut last: Option<i32> = None;
    for (tick, candidates) in events {
        if tick < end {
            continue;
        }
        let mut best = None;
        for i in candidates {
            if taken[i] {
                continue;
            }
            let n = &notes[i];
            // 현재 공격의 움직임에 미래 음높이 변화를 사용하지 않습니다.
            let (velocity, activity) = if options.layout == Layout::Learned {
                (n.original_velocity, n.onset_activity)
            } else {
                (n.note.vel, n.activity)
            };
            let mut value = 3.0 * f64::from(velocity)
                + 40.0 * activity
                + 0.5 * n.note.duration().min(192) as f64 / 24.0;
            match kind {
                LineKind::Lead => value += options.lead_pitch * f64::from(n.note.pitch),
                LineKind::Bass => value -= options.lead_pitch * f64::from(n.note.pitch),
                LineKind::Mid => {}
            }
            if let Some(previous) = last {
                value -=
                    options.lead_continuity * (f64::from(n.note.pitch) - f64::from(previous)).abs();
            }
            if best.is_none_or(|(_, old)| value > old) {
                best = Some((i, value));
            }
        }
        if let Some((i, _)) = best {
            line.push(i);
            taken[i] = true;
            end = notes[i].note.off;
            last = Some(notes[i].note.pitch);
        }
    }
    line
}

/// 원본 트랙과 음역을 이용해 양손의 음표를 나눕니다.
fn split_hands(notes: &[WorkingNote], kept: &[usize], score: &Score) -> (Vec<usize>, Vec<usize>) {
    if kept.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut names = BTreeMap::new();
    for &i in kept {
        let track = notes[i].note.src.0;
        let name = score
            .tracks
            .get(track)
            .and_then(|track| track.meta.get("name"))
            .cloned()
            .unwrap_or_else(|| track.to_string());
        let next = groups.len();
        let group = *names.entry(name).or_insert(next);
        if group == groups.len() {
            groups.push(Vec::new());
        }
        groups[group].push(i);
    }
    let mut upper = vec![false; notes.len()];
    if groups.len() < 2 {
        let mut pitches: Vec<_> = kept.iter().map(|&i| notes[i].note.pitch).collect();
        pitches.sort_unstable();
        let median = pitches[pitches.len() / 2];
        for &i in kept {
            upper[i] = notes[i].note.pitch >= median;
        }
    } else {
        let mut order: Vec<_> = (0..groups.len()).collect();
        order.sort_by(|&a, &b| mean(notes, &groups[b]).total_cmp(&mean(notes, &groups[a])));
        for &group in order.iter().take((order.len() / 2).max(1)) {
            for &i in &groups[group] {
                upper[i] = true;
            }
        }
    }
    kept.iter().copied().partition(|&i| upper[i])
}

/// 선율·내성·베이스 역할별로 음표의 기본 목적지를 정합니다.
fn musical_homes(
    notes: &[WorkingNote],
    kept: &[usize],
    score: &Score,
    options: &FoldOptions,
) -> Homes {
    let slots = options.tracks * options.parts;
    let mut taken = vec![false; notes.len()];
    let mut lines = vec![Vec::new(); slots];
    if options.parts == 1 {
        // 한 파트에 선율과 베이스를 연속 추출하면 선율이 덮어써지므로 역할을 하나만 정합니다.
        for line in &mut lines {
            *line = extract_line(notes, kept, LineKind::Lead, &mut taken, options);
        }
    } else if options.layout == Layout::Hands {
        let (upper, lower) = split_hands(notes, kept, score);
        for track in 0..options.tracks {
            lines[track * options.parts] =
                extract_line(notes, &upper, LineKind::Lead, &mut taken, options);
            lines[track * options.parts + options.parts - 1] =
                extract_line(notes, &lower, LineKind::Bass, &mut taken, options);
            for part in 1..options.parts - 1 {
                lines[track * options.parts + part] =
                    extract_line(notes, kept, LineKind::Mid, &mut taken, options);
            }
        }
    } else {
        for track in 0..options.tracks {
            lines[track * options.parts] =
                extract_line(notes, kept, LineKind::Lead, &mut taken, options);
        }
        for track in 0..options.tracks {
            lines[track * options.parts + options.parts - 1] =
                extract_line(notes, kept, LineKind::Bass, &mut taken, options);
        }
        let inner: Vec<_> = (0..options.tracks)
            .flat_map(|track| (1..options.parts - 1).map(move |part| track * options.parts + part))
            .collect();
        if !inner.is_empty() {
            let mut rest: Vec<_> = kept.iter().copied().filter(|&i| !taken[i]).collect();
            rest.sort_by(|&a, &b| notes[b].note.pitch.cmp(&notes[a].note.pitch));
            let per = rest.len().div_ceil(inner.len());
            if per > 0 {
                for (slot, band) in inner.into_iter().zip(rest.chunks(per)) {
                    lines[slot] = band.to_vec();
                }
            }
        }
    }
    let mut homes = Homes {
        home: vec![None; notes.len()],
        centre: lines.iter().map(|line| mean(notes, line)).collect(),
        primary_lead: (options.layout == Layout::Roles).then(|| lines[0].clone()),
        roles: Some(
            (0..slots)
                .map(|slot| {
                    let part = slot % options.parts;
                    if options.parts == 1 {
                        "lead"
                    } else if part == 0 {
                        if options.layout == Layout::Hands {
                            "right"
                        } else {
                            "lead"
                        }
                    } else if part == options.parts - 1 {
                        if options.layout == Layout::Hands {
                            "left"
                        } else {
                            "bass"
                        }
                    } else {
                        "inner"
                    }
                })
                .collect(),
        ),
    };
    for (slot, line) in lines.iter().enumerate() {
        for &i in line {
            homes.home[i] = Some(slot);
        }
    }
    let spare_start = if options.layout == Layout::Hands && options.tracks > 1 {
        options.parts
    } else {
        0
    };
    for &i in kept {
        if homes.home[i].is_none() {
            homes.home[i] = Some(nearest(spare_start..slots, |slot| {
                (homes.centre[slot] - f64::from(notes[i].note.pitch)).abs()
            }));
        }
    }
    homes
}

struct Packing {
    out: Vec<Vec<usize>>,
    destination: Vec<Option<usize>>,
    free_at: Vec<Tick>,
    last: Vec<Option<usize>>,
}

impl Packing {
    /// 파트의 이전 음과 새 공격 사이에 표현 가능한 쉼표가 생기는지 확인합니다.
    fn can_encode_start(&self, slot: usize, tick: Tick) -> bool {
        let end = if self.last[slot].is_some() {
            self.free_at[slot]
        } else {
            0
        };
        let gap = tick.saturating_sub(end);
        gap == 0 || gap >= MIN_TICK
    }

    /// 음표를 파트에 추가하고 다음 배치 상태를 갱신합니다.
    fn place(&mut self, i: usize, slot: usize, notes: &[WorkingNote]) {
        self.out[slot].push(i);
        self.destination[i] = Some(slot);
        self.free_at[slot] = notes[i].note.off;
        self.last[slot] = Some(i);
    }

    /// 후보 음이 기존 성부의 다음 공격 자리를 막는지 확인합니다.
    fn blocks_continuation(
        &self,
        slot: usize,
        i: usize,
        notes: &[WorkingNote],
        following: &[Option<usize>],
    ) -> bool {
        self.last[slot]
            .and_then(|last| following[last])
            .is_some_and(|next| {
                let n = &notes[i].note;
                let next_note = &notes[next].note;
                self.destination[next].is_none()
                    && next_note.src != n.src
                    && n.on <= next_note.on
                    && next_note.on < n.off
            })
    }
}

/// 기존 성부의 연결을 우선하며 단선율 파트로 배치합니다.
fn repack(
    notes: &[WorkingNote],
    kept: &[usize],
    homes: &Homes,
) -> Result<(Vec<Vec<usize>>, usize)> {
    let slots = homes.centre.len();
    let mut order = kept.to_vec();
    order.sort_by(|&a, &b| {
        notes[a]
            .note
            .on
            .cmp(&notes[b].note.on)
            .then_with(|| notes[b].note.pitch.cmp(&notes[a].note.pitch))
    });
    // 삭제된 중간 음을 건너뛰지 않고 96틱 이내의 원본 이웃만 연결합니다.
    let mut previous = vec![None; notes.len()];
    let mut following = vec![None; notes.len()];
    let mut included = vec![false; notes.len()];
    for &i in kept {
        included[i] = true;
    }
    for (i, pair) in notes.windows(2).enumerate() {
        let (a, b) = (&pair[0].note, &pair[1].note);
        if a.src == b.src
            && included[i]
            && included[i + 1]
            && a.duration() >= MIN_TICK
            && b.duration() >= MIN_TICK
            && b.on.saturating_sub(a.off) <= WHOLE / 4
        {
            previous[i + 1] = Some(i);
            following[i] = Some(i + 1);
        }
    }
    let mut packing = Packing {
        out: vec![Vec::new(); slots],
        destination: vec![None; notes.len()],
        free_at: vec![-1; slots],
        last: vec![None; slots],
    };
    let primary_line = homes.primary_lead.as_deref().unwrap_or(&[]);
    let mut primary = vec![false; notes.len()];
    for &i in primary_line {
        primary[i] = true;
    }
    let mut start = 0;
    while start < order.len() {
        let tick = notes[order[start]].note.on;
        let end = start + order[start..].partition_point(|&i| notes[i].note.on == tick);
        let batch = &order[start..end];

        // Roles의 주선율은 원본 파트가 바뀌어도 이전 파트 소유권보다 우선합니다.
        let next_primary = primary_line
            .get(primary_line.partition_point(|&i| notes[i].note.on < tick))
            .map(|&i| notes[i].note.on);
        let blocks_primary = |slot: usize, i: usize| {
            slot == 0 && !primary[i] && next_primary.is_some_and(|on| on < notes[i].note.off)
        };
        for &i in batch {
            if primary[i] && packing.free_at[0] <= tick && packing.can_encode_start(0, tick) {
                packing.place(i, 0, notes);
            }
        }

        let mut continuations: Vec<_> = batch
            .iter()
            .filter_map(|&i| {
                if packing.destination[i].is_some() {
                    return None;
                }
                let prev = previous[i]?;
                let slot = packing.destination[prev]?;
                Some((packing.last[slot] != Some(prev), i, slot))
            })
            .collect();
        continuations.sort_by_key(|&(older, _, _)| older);
        for (_, i, slot) in continuations {
            if packing.free_at[slot] <= tick
                && packing.can_encode_start(slot, tick)
                && !blocks_primary(slot, i)
            {
                packing.place(i, slot, notes);
            }
        }

        for &i in batch {
            if packing.destination[i].is_some() {
                continue;
            }
            let home = homes.home[i].context("Internal error: a kept note has no home slot")?;
            if packing.free_at[home] <= tick
                && packing.can_encode_start(home, tick)
                && !blocks_primary(home, i)
                && !packing.blocks_continuation(home, i, notes, &following)
            {
                packing.place(i, home, notes);
            }
        }
        for &i in batch {
            if packing.destination[i].is_some() {
                continue;
            }
            let n = &notes[i].note;
            let available: Vec<_> = (0..slots)
                .filter(|&slot| packing.free_at[slot] <= tick)
                .collect();
            ensure!(
                !available.is_empty(),
                "Polyphony exceeds output part count at tick {}",
                tick
            );
            let can_encode = available
                .iter()
                .any(|&slot| packing.can_encode_start(slot, tick));
            let available: Vec<_> = available
                .into_iter()
                .filter(|&slot| !can_encode || packing.can_encode_start(slot, tick))
                .collect();
            let can_preserve_lead = available.iter().any(|&slot| !blocks_primary(slot, i));
            let available: Vec<_> = available
                .into_iter()
                .filter(|&slot| !can_preserve_lead || !blocks_primary(slot, i))
                .collect();
            let can_preserve_rest = available
                .iter()
                .any(|&slot| !packing.blocks_continuation(slot, i, notes, &following));
            let slot = nearest(
                available.into_iter().filter(|&slot| {
                    !can_preserve_rest || !packing.blocks_continuation(slot, i, notes, &following)
                }),
                |slot| {
                    let last_pitch =
                        packing.last[slot].map_or(n.pitch, |last| notes[last].note.pitch);
                    (homes.centre[slot] - f64::from(n.pitch)).abs()
                        + 0.5 * (f64::from(last_pitch) - f64::from(n.pitch)).abs()
                },
            );
            packing.place(i, slot, notes);
        }
        start = end;
    }
    let spill = kept
        .iter()
        .filter(|&&i| packing.destination[i] != homes.home[i])
        .count();
    Ok((packing.out, spill))
}

/// 성부 연결 배치가 실패했을 때 음표 구간을 유지하는 대체 배치를 수행합니다.
fn legacy_repack(
    notes: &[WorkingNote],
    kept: &[usize],
    homes: &Homes,
) -> Result<(Vec<Vec<usize>>, usize)> {
    let slots = homes.centre.len();
    let mut order = kept.to_vec();
    order.sort_by_key(|&i| (notes[i].note.on, std::cmp::Reverse(notes[i].note.pitch)));
    let mut packing = Packing {
        out: vec![Vec::new(); slots],
        destination: vec![None; notes.len()],
        free_at: vec![-1; slots],
        last: vec![None; slots],
    };
    let mut spill = 0;
    for i in order {
        let n = &notes[i].note;
        let home = homes.home[i].context("Internal error: a kept note has no home slot")?;
        let slot = if packing.free_at[home] <= n.on {
            home
        } else {
            ensure!(
                packing.free_at.iter().any(|&end| end <= n.on),
                "Polyphony exceeds output part count at tick {}",
                n.on
            );
            spill += 1;
            nearest(
                (0..slots).filter(|&slot| packing.free_at[slot] <= n.on),
                |slot| {
                    let last_pitch =
                        packing.last[slot].map_or(n.pitch, |last| notes[last].note.pitch);
                    (homes.centre[slot] - f64::from(n.pitch)).abs()
                        + 0.5 * (f64::from(last_pitch) - f64::from(n.pitch)).abs()
                },
            )
        };
        packing.place(i, slot, notes);
    }
    Ok((packing.out, spill))
}

/// 메타데이터의 박자표를 마디 길이 정보로 변환합니다.
fn bar_map(tail: &[String]) -> Vec<(Tick, Tick)> {
    let mut active = false;
    let mut signatures = BTreeMap::new();
    for line in tail {
        let line = line.trim();
        if line.starts_with('[') {
            active = line == "[time-signature]";
            continue;
        }
        if !active {
            continue;
        }
        let Some((tick, signature)) = line.split_once('=') else {
            continue;
        };
        let Some((num, den)) = signature.split_once('/') else {
            continue;
        };
        let (Ok(tick), Ok(num), Ok(den)) = (
            tick.trim().parse::<Tick>(),
            num.trim().parse::<Tick>(),
            den.trim().parse::<Tick>(),
        ) else {
            continue;
        };
        if tick < 0 || num <= 0 || den <= 0 || den > WHOLE {
            continue;
        }
        if let Some(length) = num.checked_mul(WHOLE / den) {
            signatures.insert(tick, length);
        }
    }
    signatures.entry(0).or_insert(WHOLE);
    signatures.into_iter().collect()
}

/// 지정한 틱이 속한 마디 번호를 구합니다.
fn bar_of(tick: Tick, signatures: &[(Tick, Tick)]) -> Tick {
    let mut bar: Tick = 1;
    for (i, &(start, length)) in signatures.iter().enumerate() {
        match signatures.get(i + 1) {
            Some(&(end, _)) if tick >= end => {
                bar = bar.saturating_add((end - start) / length);
            }
            _ => return bar.saturating_add((tick - start) / length),
        }
    }
    bar
}

/// 배치된 파트에 음량 설정을 적용하고 MML 트랙으로 인코딩합니다.
fn encode_slots(
    notes: &[WorkingNote],
    kept: &[usize],
    slots: &[Vec<usize>],
    score: &Score,
    options: &FoldOptions,
) -> Result<(Score, Vec<usize>)> {
    let count = options.tracks * options.parts;
    let mut order: Vec<_> = (0..count).collect();
    if options.layout == Layout::Voices {
        let pitch_mean = |slot: usize| {
            if slots[slot].is_empty() {
                0.0
            } else {
                mean(notes, &slots[slot])
            }
        };
        order.sort_by(|&a, &b| pitch_mean(b).total_cmp(&pitch_mean(a)));
    }
    let tempos = score.tempos();
    let total = kept.iter().map(|&i| notes[i].note.off).max().unwrap_or(0);
    let prototype = score.tracks.first().map(|track| &track.meta);
    let mut output = Score {
        head: score.head.clone(),
        tracks: Vec::with_capacity(options.tracks),
        tail: score.tail.clone(),
    };
    for track in 0..options.tracks {
        let mut texts = Vec::with_capacity(options.parts);
        for part in 0..options.parts {
            let slot = order[track * options.parts + part];
            let ns: Vec<_> = slots[slot]
                .iter()
                .map(|&i| {
                    let mut note = notes[i].note.clone();
                    // 선택·배치가 끝난 복제본에만 한 번 적용하여 재인코딩 때 중복 압축하지 않습니다.
                    if let Some(range) = options.volume_range {
                        note.vel = range.compress(note.vel);
                    }
                    note
                })
                .collect();
            let tempo = if part == 0 { tempos.as_slice() } else { &[] };
            texts.push(emit_part(&ns, tempo, total).with_context(|| {
                format!("Cannot encode output track {} part {}", track + 1, part + 1)
            })?);
        }
        let mut meta = BTreeMap::new();
        meta.insert("name".to_owned(), format!("Track{}", track + 1));
        for key in ["program", "songProgram", "panpot", "visible"] {
            meta.insert(
                key.to_owned(),
                prototype
                    .and_then(|meta| meta.get(key))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        output.tracks.push(track_from_mml(
            format!("MML@{};", texts.join(",")),
            meta,
            track,
        )?);
    }
    Ok((output, order))
}

/// 원본을 변경하지 않고 지정한 편성으로 편곡합니다.
pub fn fold_score(score: &Score, options: &FoldOptions) -> Result<FoldResult> {
    if let Some(range) = options.volume_range {
        ensure!(
            (1..=15).contains(&range.min) && (range.min..=15).contains(&range.max),
            "Volume range must satisfy 1 <= min <= max <= 15"
        );
    }
    match fold_score_with_reduction(score, options, options.layout == Layout::Learned) {
        Ok(result) => Ok(result),
        Err(error) if options.layout == Layout::Learned => {
            // 새 공격이 템포와 1~5틱 차이라 배치 불가능하면 원본 구간으로 축소부터 한 번 재시도합니다.
            match fold_score_with_reduction(score, options, false) {
                Ok(mut result) => {
                    result.report.push_str(
                        "\nreduction : original interval priorities to preserve exact MML timing",
                    );
                    Ok(result)
                }
                Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// 동시발음 축소 정책에 따라 편곡과 결과 통계를 생성합니다.
fn fold_score_with_reduction(
    score: &Score,
    options: &FoldOptions,
    attack_priority: bool,
) -> Result<FoldResult> {
    ensure!(
        (1..=16).contains(&options.tracks),
        "Output tracks must be between 1 and 16"
    );
    ensure!(
        (1..=3).contains(&options.parts),
        "Parts per track must be between 1 and 3"
    );
    ensure!(
        options.lead_pitch.is_finite() && options.lead_pitch >= 0.0,
        "Lead pitch weight must be finite and non-negative"
    );
    ensure!(
        options.lead_continuity.is_finite() && options.lead_continuity >= 0.0,
        "Lead continuity weight must be finite and non-negative"
    );
    let count = options.tracks * options.parts;
    let mut notes = analyze(score)?;
    // 학습 때와 같이 음량·길이를 바꾸기 전의 원본 특징을 사용합니다.
    let importance =
        (options.layout == Layout::Learned).then(|| crate::salience::probabilities(score));
    let originals: Vec<_> = notes.iter().map(|n| n.note.clone()).collect();
    let (kept, mut stats) = reduce(&mut notes, count, attack_priority);
    let mut attack_slots = None;
    let mut attack_fallback = false;
    if let Some(probabilities) = &importance {
        let mut scheduled: Vec<_> = notes.iter().map(|n| n.note.clone()).collect();
        let result = crate::attack_selection::schedule(
            &mut scheduled,
            &kept,
            &crate::attack_selection::Policy {
                originals: &originals,
                probabilities,
                anchors: &attack_anchors(&notes, &originals, &kept, options),
                parts: options.parts,
                count,
                tempos: &score.tempos(),
            },
        );
        match result {
            Ok(slots) => {
                for (working, selected) in notes.iter_mut().zip(scheduled) {
                    working.note = selected;
                }
                attack_slots = Some(slots);
            }
            Err(_) => attack_fallback = true,
        }
        stats.truncated = kept
            .iter()
            .filter(|&&i| notes[i].note.off < originals[i].off)
            .count();
    }
    stats.volume_shift = if kept.is_empty() {
        0
    } else {
        match options.gain {
            Gain::None => 0,
            Gain::Auto => kept
                .iter()
                .map(|&i| notes[i].note.vel)
                .max()
                .map_or(0, |top| 15 - top),
            Gain::Shift(shift) => shift,
        }
    };
    let mut clipped = 0;
    for &i in &kept {
        let value = i64::from(notes[i].note.vel) + i64::from(stats.volume_shift);
        let bounded = value.clamp(0, 15);
        if value != bounded {
            clipped += 1;
        }
        notes[i].note.vel = bounded as i32;
    }
    let homes = match options.layout {
        Layout::Voices => voice_homes(&notes, &kept, count),
        Layout::Hands | Layout::Roles => musical_homes(&notes, &kept, score, options),
        Layout::Learned => match &attack_slots {
            Some(slots) => slot_homes(&notes, slots),
            None => learned_homes(&notes, &kept, importance.as_deref().unwrap(), options)?,
        },
    };
    let pack = |legacy| {
        if options.layout == Layout::Learned {
            repack_tracks(&notes, &kept, &homes, options.parts, legacy)
        } else if legacy {
            legacy_repack(&notes, &kept, &homes)
        } else {
            repack(&notes, &kept, &homes)
        }
    };
    let (mut slots, mut spill) = match attack_slots {
        Some(slots) => (slots, 0),
        None => pack(false)?,
    };
    let (output, order, exact_timing_fallback) =
        match encode_slots(&notes, &kept, &slots, score, options) {
            Ok((output, order)) => (output, order, false),
            Err(_) => {
                // 파트 이동으로 1~5틱 조각이 생기면 공격을 보정하지 않고 기존의 정확한 배치를 재시도합니다.
                (slots, spill) = pack(true)?;
                let (output, order) = encode_slots(&notes, &kept, &slots, score, options)?;
                (output, order, true)
            }
        };
    stats.spill = spill;
    let mut report = vec![
        format!("notes in  : {}", stats.total),
        format!("kept      : {}", stats.kept),
        format!(
            "truncated : {}  (attack kept, tail shortened)",
            stats.truncated
        ),
        format!("dropped   : {}", stats.dropped),
        format!(
            "voice spill: {} notes ({:.2}%) used another slot for continuity or polyphony",
            stats.spill,
            100.0 * stats.spill as f64 / stats.kept.max(1) as f64
        ),
    ];
    if exact_timing_fallback {
        report.push(if options.layout == Layout::Learned {
            "placement : alternate part packing to preserve exact MML timing".to_owned()
        } else {
            "placement : original interval packing to preserve exact MML timing".to_owned()
        });
    }
    if attack_fallback {
        report.push("core attacks: exact-tick fallback retained complete intervals".to_owned());
    } else if options.layout == Layout::Learned {
        report.push(
            "core attacks: long sounding tails yield to new notes; attacks unchanged".to_owned(),
        );
    }
    if options.gain != Gain::None {
        if let Some(top) = kept.iter().map(|&i| notes[i].note.vel).max() {
            let dynamics = if clipped == 0 && options.volume_range.is_some() {
                "no shift clipping".to_owned()
            } else if clipped == 0 {
                "dynamic range unchanged".to_owned()
            } else {
                format!("{clipped} notes clipped to v0..v15")
            };
            let stage = if options.volume_range.is_some() {
                "before DR"
            } else {
                "now"
            };
            report.push(format!(
                "volume shift: {:+}  (loudest note {stage} v{}, {})",
                stats.volume_shift, top, dynamics
            ));
        } else {
            report.push("volume shift: +0  (no notes)".to_owned());
        }
    }
    if let Some(range) = options.volume_range {
        report.push(format!(
            "volume DR : v1..v15 -> v{}..v{} (after shift; v0 unchanged)",
            range.min, range.max
        ));
    }
    report.push(format!("layout    : {}", options.layout.name()));
    if let Some(primary) = &homes.primary_lead {
        let placed: std::collections::BTreeSet<_> = slots[0].iter().copied().collect();
        let anchored = primary.iter().filter(|&&i| placed.contains(&i)).count();
        report.push(format!(
            "primary lead: {anchored}/{} notes in track1 part1 ({} role spills)",
            primary.len(),
            primary.len() - anchored
        ));
    }
    for track in 0..options.tracks {
        for part in 0..options.parts {
            let slot = order[track * options.parts + part];
            let pitches: Vec<_> = slots[slot].iter().map(|&i| notes[i].note.pitch).collect();
            let label = homes
                .roles
                .as_ref()
                .map_or(String::new(), |roles| format!("  [{}]", roles[slot]));
            report.push(format!(
                "track{} part{}: {:5} notes, pitch {}..{}, mean {:.1}{}",
                track + 1,
                part,
                pitches.len(),
                pitches.iter().min().copied().unwrap_or(0),
                pitches.iter().max().copied().unwrap_or(0),
                pitches.iter().map(|&p| f64::from(p)).sum::<f64>() / pitches.len().max(1) as f64,
                label
            ));
        }
    }
    let signatures = bar_map(&score.tail);
    let mut lost = Vec::<(Tick, usize)>::new();
    let mut bar_index = BTreeMap::new();
    for n in &notes {
        if n.note.duration() < MIN_TICK {
            let bar = bar_of(n.note.on, &signatures);
            let next = lost.len();
            let position = *bar_index.entry(bar).or_insert(next);
            if position == lost.len() {
                lost.push((bar, 0));
            }
            lost[position].1 += 1;
        }
    }
    if !lost.is_empty() {
        report.push(String::new());
        report.push("bars where most notes were removed (listen here first):".to_owned());
        let mut bars: Vec<_> = lost.iter().collect();
        bars.sort_by_key(|&&(_, count)| std::cmp::Reverse(count));
        for (bar, count) in bars.into_iter().take(15) {
            report.push(format!("   bar {bar:<5} {count} notes"));
        }
        report.push(format!("bars touched at all: {}", lost.len()));
    }
    Ok(FoldResult {
        score: output,
        report: report.join("\n"),
        stats,
    })
}
