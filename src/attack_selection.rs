//! 공격 시점을 유지하면서 핵심 성부의 긴 꼬리를 새 음에 양보하도록 배치합니다.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};

use crate::core::split_ticks;
use crate::{Note, Tempo, Tick};

/// 음표 길이와 템포 사이 조각이 모두 정확히 표현되는지 확인합니다.
fn exact_span(start: Tick, end: Tick, tempos: &[Tempo]) -> bool {
    if end < start || split_ticks(end - start).is_err() {
        return false;
    }
    let mut previous = start;
    for &(tick, _) in tempos.iter().skip_while(|&&(tick, _)| tick <= start) {
        if tick >= end {
            break;
        }
        if split_ticks(tick - previous).is_err() {
            return false;
        }
        previous = tick;
    }
    split_ticks(end - previous).is_ok()
}

/// 남은 공격들을 중복 없는 파트에 배치할 수 있는지 확인합니다.
fn fits(edges: &[Vec<usize>], first: usize, reserved: &[bool]) -> bool {
    /// 증가 경로로 파트 소유자를 옮겨 한 공격의 자리를 확보합니다.
    fn augment(
        note: usize,
        edges: &[Vec<usize>],
        reserved: &[bool],
        seen: &mut [bool],
        owners: &mut [Option<usize>],
    ) -> bool {
        for &slot in &edges[note] {
            if reserved[slot] || seen[slot] {
                continue;
            }
            seen[slot] = true;
            if owners[slot].is_none_or(|other| augment(other, edges, reserved, seen, owners)) {
                owners[slot] = Some(note);
                return true;
            }
        }
        false
    }
    let mut owners = vec![None; reserved.len()];
    (first..edges.len()).all(|note| {
        augment(
            note,
            edges,
            reserved,
            &mut vec![false; reserved.len()],
            &mut owners,
        )
    })
}

/// 명확한 넓은 화음에서 선율·베이스·화성음을 함께 남길 후보를 고릅니다.
fn chord_edges(
    notes: &[Note],
    batch: &[usize],
    entry: &[f64],
    edges: &[Vec<usize>],
    slots: &[Vec<usize>],
    policy: &Policy<'_>,
    harmony: Option<crate::harmony::Harmony>,
) -> Option<Vec<Vec<usize>>> {
    if policy.parts != 3 || batch.len() <= 3 || policy.count <= 3 {
        return None;
    }
    let first = &notes[batch[0]];
    if slots
        .iter()
        .any(|part| part.last().is_some_and(|&i| notes[i].off > first.on))
    {
        return None;
    }
    let pitches: Vec<_> = batch.iter().map(|&i| notes[i].pitch).collect();
    let bottom = *pitches.iter().min()?;
    let top = *pitches.iter().max()?;
    if top - bottom < 24 {
        return None;
    }
    let harmony = harmony?;
    let classes = pitches
        .iter()
        .fold(0u16, |mask, pitch| mask | (1 << pitch.rem_euclid(12)));
    if classes.count_ones() < 3 {
        return None;
    }
    let root = i32::from(harmony.root());
    let fifth = (root + 7) % 12;
    let plain_triad = harmony.mask().count_ones() == 3 && harmony.mask() & (1 << fifth) != 0;
    // 동음 사본은 가장 긴 길이 하나로 묶어 서로 다른 상성부의 기준을 보존합니다.
    let mut pitch_lengths = BTreeMap::<i32, Tick>::new();
    for &i in batch {
        pitch_lengths
            .entry(notes[i].pitch)
            .and_modify(|length| *length = (*length).max(notes[i].duration()))
            .or_insert(notes[i].duration());
    }
    let mut upper_lengths: Vec<_> = pitch_lengths.values().rev().take(3).copied().collect();
    upper_lengths.sort_unstable();
    let upper_length = upper_lengths[1] as f64;
    let mut probabilities: Vec<_> = batch.iter().map(|&i| policy.probabilities[i]).collect();
    probabilities.sort_by(|a, b| b.total_cmp(a));
    let separation = probabilities[..3].iter().sum::<f64>() / 3.0
        - probabilities[3..].iter().sum::<f64>() / (probabilities.len() - 3) as f64;
    // 보정항은 확률 재보정이 아니라 모델 판단이 모호할 때만 커지는 제한된 가산점입니다.
    let ambiguity = 1.0 - separation.clamp(0.0, 1.0);
    let mut best: Option<(f64, Vec<Vec<usize>>)> = None;
    for a in 0..batch.len() - 2 {
        for b in a + 1..batch.len() - 1 {
            for c in b + 1..batch.len() {
                let chosen = [a, b, c];
                let mut selected = [pitches[a], pitches[b], pitches[c]];
                selected.sort_unstable();
                let bass_length = chosen
                    .iter()
                    .map(|&j| &notes[batch[j]])
                    .filter(|note| note.pitch == selected[0])
                    .map(Note::duration)
                    .max()
                    .unwrap() as f64;
                // 짧은 베이스에는 상성부 길이 대비 지속 비율만큼만 저음 보너스를 줍니다.
                let bass = (-f64::from(selected[0] - bottom) / 12.0).exp()
                    * (bass_length / upper_length).min(1.0);
                let lead = (-f64::from(top - selected[2]) / 12.0).exp();
                let skeleton = f64::from(selected.iter().any(|p| p.rem_euclid(12) == root))
                    + f64::from(selected.iter().any(|p| p.rem_euclid(12) == fifth));
                let lead_octave =
                    f64::from(selected.contains(&top) && selected.contains(&(top - 12)));
                let middle_class = selected[1].rem_euclid(12);
                let upper_support = if middle_class == root || middle_class == fifth {
                    f64::from(selected[1] - bottom) / f64::from(top - bottom)
                } else {
                    0.0
                };
                let unisons = selected
                    .windows(2)
                    .filter(|pair| pair[0] == pair[1])
                    .count() as f64;
                let selected_mask = selected
                    .iter()
                    .fold(0u16, |mask, pitch| mask | (1 << pitch.rem_euclid(12)));
                let familiar_voicing = if plain_triad {
                    0.10 * skeleton + 0.15 * lead_octave + 0.05 * upper_support
                } else {
                    0.0
                };
                let value = chosen.iter().map(|&j| entry[batch[j]]).sum::<f64>() / 3.0
                    + ambiguity
                        * (0.25 * bass + 0.10 * lead + familiar_voicing - 0.15 * unisons
                            + 0.04 * harmony.coverage(selected_mask) / 3.0);
                if best.as_ref().is_some_and(|(old, _)| value <= *old) {
                    continue;
                }
                let candidate: Vec<Vec<usize>> = edges
                    .iter()
                    .enumerate()
                    .map(|(j, available)| {
                        available
                            .iter()
                            .copied()
                            .filter(|&slot| (slot < policy.parts) == chosen.contains(&j))
                            .collect()
                    })
                    .collect();
                if fits(&candidate, 0, &vec![false; policy.count]) {
                    best = Some((value, candidate));
                }
            }
        }
    }
    best.map(|(_, candidate)| candidate)
}

#[derive(Clone, Copy)]
struct Handoff {
    lead: usize,
    support: usize,
    held_slot: usize,
}

/// 상성부 진행과 아래 화음의 새 공격을 함께 보존할 교체 조합을 찾습니다.
fn bass_chord_handoff(
    notes: &[Note],
    batch: &[usize],
    slots: &[Vec<usize>],
    policy: &Policy<'_>,
) -> Option<Handoff> {
    if policy.parts != 3 || policy.count <= policy.parts {
        return None;
    }
    // 가장 높은 진입 점수의 새 음이 최고음일 때만 상성부 진행으로 인정합니다.
    let &lead = batch.first()?;
    let top = &notes[lead];
    if batch.iter().any(|&i| notes[i].pitch > top.pitch) {
        return None;
    }
    let tick = top.on;
    let core: Vec<_> = slots[..policy.parts]
        .iter()
        .enumerate()
        .filter_map(|(slot, history)| {
            history
                .last()
                .copied()
                .filter(|&i| notes[i].on < tick && notes[i].off >= tick)
                .map(|i| (slot, i))
        })
        .collect();
    let &(_, bass) = core.iter().min_by_key(|&&(_, i)| notes[i].pitch)?;
    let bass = &notes[bass];
    let &(recent_slot, recent) = core.iter().max_by_key(|&&(_, i)| notes[i].on)?;
    let recent = &notes[recent];
    if recent.on <= bass.on {
        return None;
    }
    // 베이스 뒤의 실제 상성부 이동과 같은 방향 진행 또는 재타격이 있어야 합니다.
    let repeated = top.pitch == recent.pitch;
    let &(first_slot, first) = core.iter().find(|&&(_, i)| {
        let first = &notes[i];
        first.on == bass.on
            && i64::from(first.pitch) - i64::from(bass.pitch) >= 12
            && (i64::from(recent.pitch) - i64::from(first.pitch)).signum() != 0
            && (repeated
                || (i64::from(recent.pitch) - i64::from(first.pitch)).signum()
                    == (i64::from(top.pitch) - i64::from(recent.pitch)).signum())
    })?;
    let (held_slot, held) = if repeated {
        (first_slot, &notes[first])
    } else {
        (recent_slot, recent)
    };
    if held.off <= tick {
        return None;
    }
    let lower_ceiling = i64::from(recent.pitch.min(top.pitch)) - 12;
    let minimum_duration = if repeated {
        top.duration()
    } else {
        recent.off - tick
    }
    .clamp(24, 96);
    let reference_volume = bass.vel.max(recent.vel).max(top.vel);
    let lower: Vec<_> = batch
        .iter()
        .copied()
        .filter(|&i| {
            let note = &notes[i];
            note.pitch > bass.pitch
                && i64::from(note.pitch) <= lower_ceiling
                && note.duration() >= minimum_duration
                && note.vel > 0
                && note.vel * 2 >= reference_volume
        })
        .collect();
    let distinct: std::collections::BTreeSet<_> = lower.iter().map(|&i| notes[i].pitch).collect();
    if distinct.len() < 2 {
        return None;
    }
    let support = *lower.iter().min_by_key(|&&i| (notes[i].pitch, i))?;
    Some(Handoff {
        lead,
        support,
        held_slot,
    })
}

pub(crate) struct Policy<'a> {
    pub(crate) originals: &'a [Note],
    pub(crate) probabilities: &'a [f64],
    pub(crate) anchors: &'a [bool],
    pub(crate) parts: usize,
    pub(crate) count: usize,
    pub(crate) tempos: &'a [Tempo],
}

/// 강한 원본 선율의 지속음은 무관한 아래 성부보다 빈 보조 파트를 먼저 사용하게 합니다.
fn held_source_lead(
    held: usize,
    incoming: usize,
    predecessor: &[Option<usize>],
    successor: &[Option<usize>],
    policy: &Policy<'_>,
) -> bool {
    let note = &policy.originals[held];
    let next = &policy.originals[incoming];
    if note.src == next.src
        || i64::from(note.pitch) - i64::from(next.pitch) < 12
        || policy.probabilities[held] < 0.85
        || policy.probabilities[held] + 0.05 < policy.probabilities[incoming]
    {
        return false;
    }
    [predecessor[held], successor[held]]
        .into_iter()
        .flatten()
        .any(|neighbor| {
            let adjacent = &policy.originals[neighbor];
            let gap = if adjacent.on < note.on {
                note.on - adjacent.off
            } else {
                adjacent.on - note.off
            };
            (0..=96).contains(&gap)
                && (i64::from(adjacent.pitch) - i64::from(note.pitch)).abs() <= 12
        })
}

/// 공격별 우선도와 정확한 음가 제약에 따라 핵심 및 보조 파트에 배치합니다.
pub(crate) fn schedule(
    notes: &mut [Note],
    kept: &[usize],
    policy: &Policy<'_>,
) -> Result<Vec<Vec<usize>>> {
    let mut slots: Vec<Vec<usize>> = vec![Vec::new(); policy.count];
    let mut core = vec![false; notes.len()];
    let mut entry = vec![0.0; notes.len()];
    let mut predecessor = vec![None; notes.len()];
    let mut successor = vec![None; notes.len()];
    let mut previous = BTreeMap::new();
    // 탈락한 원본 음도 이웃 계산에 포함해 그 음을 건너 연결하지 않습니다.
    for (i, note) in policy.originals.iter().enumerate() {
        predecessor[i] = previous.insert(note.src, i);
        if let Some(previous) = predecessor[i] {
            successor[previous] = Some(i);
        }
    }
    let mut events: BTreeMap<Tick, Vec<usize>> = BTreeMap::new();
    for &i in kept {
        events.entry(notes[i].on).or_default().push(i);
    }
    // 탈락한 음도 원래 끝나는 시점까지 화음 문맥에 남깁니다.
    let mut original_events: Vec<_> = (0..policy.originals.len()).collect();
    original_events.sort_by_key(|&i| policy.originals[i].on);
    let mut original_cursor = 0;
    let mut original_sounding = Vec::new();
    for (tick, mut batch) in events {
        original_sounding.retain(|&i: &usize| policy.originals[i].off > tick);
        while original_cursor < original_events.len()
            && policy.originals[original_events[original_cursor]].on <= tick
        {
            let i = original_events[original_cursor];
            if policy.originals[i].off > tick {
                original_sounding.push(i);
            }
            original_cursor += 1;
        }
        let harmony = (batch.len() > 3)
            .then(|| {
                crate::harmony::analyze(
                    original_sounding.iter().map(|&i| policy.originals[i].pitch),
                )
            })
            .flatten();
        for &i in &batch {
            let affinity = predecessor[i].map_or(0.0, |j| {
                let gap = tick - policy.originals[j].off;
                if core[j] && (0..=96).contains(&gap) {
                    (-(gap as f64) / 96.0).exp()
                        * (-(notes[i].pitch - notes[j].pitch).abs() as f64 / 12.0).exp()
                } else {
                    0.0
                }
            });
            entry[i] = policy.probabilities[i]
                + if policy.anchors[i] { 0.15 } else { 0.0 }
                + 0.08 * affinity;
        }
        batch.sort_by(|&a, &b| entry[b].total_cmp(&entry[a]).then(a.cmp(&b)));
        let build_edges = |handoff: Option<Handoff>| {
            let mut edges = Vec::with_capacity(batch.len());
            for &i in &batch {
                let mut candidates = Vec::new();
                for (slot, history) in slots.iter().enumerate() {
                    if handoff.is_some_and(|h| {
                        slot == h.held_slot
                            || ((i == h.lead || i == h.support) && slot >= policy.parts)
                    }) {
                        continue;
                    }
                    let tempos = if slot % policy.parts == 0 {
                        policy.tempos
                    } else {
                        &[]
                    };
                    if !exact_span(tick, notes[i].off, tempos) {
                        continue;
                    }
                    let last = history.last().copied();
                    let end = last.map_or(0, |j| notes[j].off);
                    let held = end > tick;
                    let mut holding_score = 0.0;
                    if held {
                        let j = last.expect("held note");
                        let age = tick - notes[j].on;
                        holding_score = entry[j] * (0.10 + 0.90 * 2.0_f64.powf(-age as f64 / 48.0));
                        // 원래 길이 대신 이미 울린 시간을 기준으로 최소 24틱의 공격을 보호합니다.
                        if slot >= policy.parts
                        || age < 24
                        // 96틱 이상 지난 꼬리는 낮은 모델 점수만으로 새 공격의 진입을 막지 않습니다.
                        || (age < 96
                            && entry[i] <= holding_score + 0.02
                            && !handoff.is_some_and(|h| i == h.support))
                        || !exact_span(notes[j].on, tick, tempos)
                        {
                            continue;
                        }
                    } else if !exact_span(end, tick, tempos) {
                        continue;
                    }
                    let distance = last.map_or(12.0, |j| {
                        f64::from((notes[i].pitch - notes[j].pitch).abs())
                            - if predecessor[i] == Some(j) { 24.0 } else { 0.0 }
                    });
                    let tier = if slot >= policy.parts {
                        2
                    } else if held
                        && held_source_lead(
                            last.expect("held note"),
                            i,
                            &predecessor,
                            &successor,
                            policy,
                        )
                    {
                        3
                    } else {
                        usize::from(held)
                    };
                    candidates.push((slot, tier, holding_score, distance));
                }
                candidates.sort_by(|a, b| {
                    a.1.cmp(&b.1)
                        .then(a.2.total_cmp(&b.2))
                        .then(a.3.total_cmp(&b.3))
                        .then(a.0.cmp(&b.0))
                });
                edges.push(candidates.into_iter().map(|c| c.0).collect::<Vec<_>>());
            }
            edges
        };
        let mut edges = build_edges(None);
        if let Some(handoff) = bass_chord_handoff(notes, &batch, &slots, policy)
            && edges[0].iter().any(|&slot| slot < policy.parts)
        {
            let proposed = build_edges(Some(handoff));
            // 모든 새 공격의 정확한 배치가 가능할 때만 교체 조합 전체를 예약합니다.
            if fits(&proposed, 0, &vec![false; policy.count]) {
                edges = proposed;
            }
        }
        if let Some(joint) = chord_edges(notes, &batch, &entry, &edges, &slots, policy, harmony) {
            edges = joint;
        }
        let mut reserved = vec![false; policy.count];
        ensure!(
            fits(&edges, 0, &reserved),
            "No exact attack placement at tick {tick}"
        );
        for (position, &i) in batch.iter().enumerate() {
            let slot = edges[position]
                .iter()
                .copied()
                .find(|&slot| {
                    if reserved[slot] {
                        return false;
                    }
                    reserved[slot] = true;
                    let feasible = fits(&edges, position + 1, &reserved);
                    reserved[slot] = false;
                    feasible
                })
                .expect("onset matching established");
            reserved[slot] = true;
            if let Some(&j) = slots[slot].last()
                && notes[j].off > tick
            {
                notes[j].off = tick;
            }
            slots[slot].push(i);
            core[i] = slot < policy.parts;
        }
    }
    // 마지막 음 뒤에도 템포를 쓰므로 그 사이에 1~5틱 간격을 남기지 않습니다.
    if let Some(&(last_tempo, _)) = policy.tempos.last() {
        for history in slots.iter().step_by(policy.parts) {
            let end = history.last().map_or(0, |&i| notes[i].off);
            ensure!(
                end >= last_tempo || exact_span(end, last_tempo, policy.tempos),
                "Unrepresentable rest after final attack at tick {end}"
            );
        }
    }
    Ok(slots)
}

#[cfg(test)]
mod handoff_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// 배치 결과의 공격 보존과 단선율 조건을 함께 검사합니다.
    fn run(
        notes: &mut [Note],
        scores: &[f64],
        parts: usize,
        count: usize,
        tempos: &[Tempo],
    ) -> Vec<Vec<usize>> {
        let originals = notes.to_vec();
        schedule(
            notes,
            &(0..notes.len()).collect::<Vec<_>>(),
            &Policy {
                originals: &originals,
                probabilities: scores,
                anchors: &vec![false; notes.len()],
                parts,
                count,
                tempos,
            },
        )
        .unwrap()
    }

    /// 공격 배치 검사에 사용할 음표를 만듭니다.
    fn n(on: Tick, off: Tick, pitch: i32) -> Note {
        Note {
            on,
            off,
            pitch,
            vel: 8,
            src: (pitch as usize, 0),
        }
    }

    #[test]
    /// 긴 꼬리를 한 번만 자르고 다른 파트에서 재시작하지 않는지 검사합니다.
    fn long_tail_yields_once_without_restarting() {
        let mut notes = vec![n(0, 384, 43), n(96, 144, 72), n(144, 192, 74)];
        let slots = run(&mut notes, &[0.9, 0.7, 0.7], 1, 2, &[]);
        assert_eq!(slots, vec![vec![0, 1, 2], vec![]]);
        assert_eq!(notes[0].off, 96);
        assert_eq!(notes[1].on, 96);
    }

    #[test]
    /// 빈 파트와 경쟁 없는 음의 길이가 유지되는지 검사합니다.
    fn free_parts_and_uncontested_tails_are_not_cut() {
        let mut notes = vec![n(0, 384, 43), n(96, 144, 72)];
        let before = notes.clone();
        let slots = run(&mut notes, &[0.9, 0.7], 2, 3, &[]);
        assert_eq!(notes, before);
        assert_eq!(slots[0], vec![0]);
        assert_eq!(slots[1], vec![1]);
    }

    #[test]
    /// 점수가 낮은 새 공격도 한 박 지난 꼬리의 자리를 받는지 검사합니다.
    fn even_a_low_scoring_attack_can_use_a_one_beat_old_tail() {
        let mut notes = vec![n(0, 576, 43), n(96, 186, 63), n(192, 282, 61)];
        let slots = run(&mut notes, &[0.99, 0.0, 0.0], 1, 2, &[]);
        assert_eq!(slots, vec![vec![0, 1, 2], vec![]]);
        assert_eq!(notes[0].off, 96);
    }

    #[test]
    /// 짧은 음의 꼬리를 양보하면서 더 어린 공격은 보호하는지 검사합니다.
    fn eighth_note_tail_yields_without_cutting_a_younger_voice() {
        for transpose in -12..=12 {
            let mut notes = vec![
                n(0, 12, 74 + transpose),
                n(0, 48, 50 + transpose),
                n(6, 48, 55 + transpose),
                n(18, 24, 57 + transpose),
                n(24, 36, 74 + transpose),
                n(24, 144, 62 + transpose),
            ];
            let mut expected = notes.clone();
            expected[1].off = 24;
            let slots = run(
                &mut notes,
                &[0.98, 0.346, 0.481, 0.8, 0.984, 0.540],
                3,
                6,
                &[(0, 120), (24, 144)],
            );
            assert_eq!(notes, expected);
            assert!(slots[..3].iter().any(|part| part.contains(&4)));
            assert!(slots[..3].iter().any(|part| part.contains(&5)));
            for part in &slots {
                assert!(
                    part.windows(2)
                        .all(|pair| notes[pair[0]].off <= notes[pair[1]].on)
                );
            }
            let mut all: Vec<_> = slots.into_iter().flatten().collect();
            all.sort_unstable();
            assert_eq!(all, (0..notes.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    /// 어리거나 점수가 높은 기존 공격이 보호되는지 검사합니다.
    fn young_and_stronger_touches_are_protected() {
        for (off, onset, score) in [
            (24, 12, 0.9),
            (48, 23, 0.9),
            (48, 24, 0.1),
            (192, 12, 0.9),
            (192, 24, 0.1),
        ] {
            let mut notes = vec![n(0, off, 43), n(onset, onset + 24, 72)];
            let slots = run(&mut notes, &[0.9, score], 1, 2, &[]);
            assert_eq!(notes[0].off, off);
            assert_eq!(slots, vec![vec![0], vec![1]]);
        }
    }

    #[test]
    /// 절단으로 표현 불가능한 템포 조각이 생기지 않는지 검사합니다.
    fn tempo_fragment_cannot_be_created_by_cutting() {
        let mut notes = vec![n(0, 192, 43), n(36, 84, 60), n(49, 97, 72)];
        let slots = run(&mut notes, &[0.9, 0.9, 0.9], 2, 4, &[(0, 120), (48, 144)]);
        assert_eq!(notes[0].off, 192);
        assert_eq!(slots, vec![vec![0], vec![1], vec![], vec![2]]);
    }

    #[test]
    /// 동시에 시작한 동음들이 각각 한 번씩 배치되는지 검사합니다.
    fn simultaneous_unisons_are_each_placed_once() {
        let mut notes = vec![n(0, 384, 43), n(96, 144, 72), n(96, 144, 72)];
        let slots = run(&mut notes, &[0.9, 0.7, 0.7], 1, 3, &[]);
        assert_eq!(slots, vec![vec![0, 1], vec![2], vec![]]);
        assert_eq!(notes[0].off, 96);
    }

    #[test]
    /// 원본 중간 음의 누락이 성부 연결 가산점을 끊는지 검사합니다.
    fn dropped_source_note_breaks_core_affinity() {
        let originals = vec![n(0, 24, 60), n(24, 48, 60), n(48, 96, 60), n(24, 192, 40)];
        let mut notes = originals.clone();
        notes[1].off = notes[1].on;
        let slots = schedule(
            &mut notes,
            &[0, 2, 3],
            &Policy {
                originals: &originals,
                probabilities: &[0.9, 0.9, 0.64, 0.9],
                anchors: &[false; 4],
                parts: 1,
                count: 2,
                tempos: &[],
            },
        )
        .unwrap();
        assert_eq!(slots, vec![vec![0, 3], vec![2]]);
        assert_eq!(notes[3].off, 192);
    }

    #[test]
    /// 제약이 큰 공격에 유일하게 가능한 파트를 예약하는지 검사합니다.
    fn batch_matching_reserves_the_only_exact_part() {
        let edges = vec![vec![0, 1], vec![0]];
        assert!(fits(&edges, 0, &[false, false]));
        assert!(!fits(&edges, 1, &[true, false]));
        assert!(fits(&edges, 1, &[false, true]));
    }

    #[test]
    /// 전조한 넓은 화음에서도 선율·베이스·근음을 유지하는지 검사합니다.
    fn wide_block_chords_keep_melody_bass_and_root_across_transpositions() {
        for transpose in -12..=12 {
            for (pitches, probabilities, wanted) in [
                (
                    [89, 82, 77, 41, 38, 34],
                    [0.8004, 0.3013, 0.0596, 0.0054, 0.0039, 0.0201],
                    [89, 77, 34],
                ),
                (
                    [90, 86, 83, 62, 59, 54],
                    [0.2233, 0.0811, 0.0142, 0.0070, 0.0194, 0.0146],
                    [90, 83, 54],
                ),
            ] {
                let mut notes: Vec<_> = pitches.iter().map(|&p| n(0, 48, p + transpose)).collect();
                let original = notes.clone();
                let slots = run(&mut notes, &probabilities, 3, 6, &[]);
                let selected: std::collections::BTreeSet<_> = slots[..3]
                    .iter()
                    .flatten()
                    .map(|&i| notes[i].pitch)
                    .collect();
                assert_eq!(
                    selected,
                    wanted.into_iter().map(|p| p + transpose).collect()
                );
                assert_eq!(notes, original);
                let mut all: Vec<_> = slots.into_iter().flatten().collect();
                all.sort_unstable();
                assert_eq!(all, (0..6).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    /// 명확한 모델 판단을 화음 보정이 뒤집지 않는지 검사합니다.
    fn block_chord_prior_does_not_override_clear_model_evidence() {
        let mut notes: Vec<_> = [90, 86, 83, 62, 59, 54]
            .iter()
            .map(|&p| n(0, 48, p))
            .collect();
        let slots = run(&mut notes, &[0.99, 0.98, 0.01, 0.01, 0.97, 0.01], 3, 6, &[]);
        let selected: std::collections::BTreeSet<_> =
            slots[..3].iter().flatten().copied().collect();
        assert_eq!(selected, [0, 1, 4].into_iter().collect());
    }

    #[test]
    /// 음들의 종료 시점이 달라도 길이를 바꾸지 않고 화음을 선택하는지 검사합니다.
    fn unequal_releases_do_not_bypass_chord_selection_or_change_durations() {
        for lengths in [
            [96, 48, 48, 48, 48, 48],
            [336, 276, 276, 564, 564, 564],
            [48, 96, 144, 192, 240, 288],
        ] {
            let mut notes: Vec<_> = [90, 86, 83, 62, 59, 54]
                .iter()
                .zip(lengths)
                .map(|(&p, duration)| n(0, duration, p))
                .collect();
            let original = notes.clone();
            let slots = run(
                &mut notes,
                &[0.2233, 0.0811, 0.0142, 0.0070, 0.0194, 0.0146],
                3,
                6,
                &[],
            );
            let selected: std::collections::BTreeSet<_> =
                slots[..3].iter().flatten().copied().collect();
            assert_eq!(selected, [0, 2, 5].into_iter().collect());
            assert_eq!(notes, original);
        }
    }

    #[test]
    /// 짧은 상성부와 긴 베이스가 있는 화음에서 근음을 유지하는지 검사합니다.
    fn shorter_upper_voices_keep_the_chord_root_and_sustained_bass() {
        for transpose in -12..=12 {
            let mut notes: Vec<_> = [90, 86, 83, 62, 59, 54]
                .into_iter()
                .zip([336, 276, 276, 564, 564, 564])
                .map(|(pitch, length)| n(0, length, pitch + transpose))
                .collect();
            let original = notes.clone();
            let slots = run(
                &mut notes,
                &[
                    0.431460352,
                    0.232719623,
                    0.035885277,
                    0.008902867,
                    0.018615248,
                    0.013478345,
                ],
                3,
                6,
                &[(0, 120), (96, 144)],
            );
            let selected: std::collections::BTreeSet<_> =
                slots[..3].iter().flatten().copied().collect();
            assert_eq!(selected, [0, 2, 5].into_iter().collect());
            assert_eq!(notes, original);
            let mut all: Vec<_> = slots.into_iter().flatten().collect();
            all.sort_unstable();
            assert_eq!(all, (0..6).collect::<Vec<_>>());
        }
    }

    #[test]
    /// 짧은 베이스가 강한 상성부를 밀어내지 않는지 검사합니다.
    fn short_bass_attacks_do_not_displace_stronger_sustained_upper_voices() {
        let mut notes: Vec<_> = [90, 86, 83, 62, 59, 54]
            .into_iter()
            .zip([336, 276, 276, 6, 6, 6])
            .map(|(pitch, length)| n(0, length, pitch))
            .collect();
        let slots = run(
            &mut notes,
            &[
                0.431460352,
                0.232719623,
                0.035885277,
                0.008902867,
                0.018615248,
                0.013478345,
            ],
            3,
            6,
            &[],
        );
        let selected: std::collections::BTreeSet<_> =
            slots[..3].iter().flatten().copied().collect();
        assert_eq!(selected, [0, 1, 2].into_iter().collect());
    }

    #[test]
    /// 동음 사본이 상성부 길이 기준을 왜곡하지 않는지 검사합니다.
    fn unison_copies_do_not_crowd_out_the_upper_duration_reference() {
        let mut notes: Vec<_> = [90, 90, 90, 86, 83, 54]
            .into_iter()
            .zip([48, 384, 384, 48, 48, 48])
            .map(|(pitch, length)| n(0, length, pitch))
            .collect();
        let original = notes.clone();
        let slots = run(
            &mut notes,
            &[0.431, 0.431, 0.431, 0.233, 0.036, 0.013],
            3,
            6,
            &[],
        );
        let selected: std::collections::BTreeSet<_> = slots[..3]
            .iter()
            .flatten()
            .map(|&i| notes[i].pitch)
            .collect();
        assert_eq!(selected, [90, 83, 54].into_iter().collect());
        assert_eq!(notes, original);
        let mut all: Vec<_> = slots.into_iter().flatten().collect();
        all.sort_unstable();
        assert_eq!(all, (0..6).collect::<Vec<_>>());
    }

    #[test]
    /// 서로 다른 음이 들어갈 때 동음 사본이 핵심 파트를 중복 점유하지 않는지 검사합니다.
    fn exact_unisons_do_not_take_two_core_parts_when_distinct_tones_fit() {
        let mut notes: Vec<_> = [36, 60, 72, 76, 79, 79]
            .iter()
            .map(|&p| n(0, 48, p))
            .collect();
        let slots = run(&mut notes, &[0.4; 6], 3, 6, &[]);
        let selected: std::collections::BTreeSet<_> = slots[..3]
            .iter()
            .flatten()
            .map(|&i| notes[i].pitch)
            .collect();
        assert_eq!(selected.len(), 3);
        assert!(selected.contains(&36) && selected.contains(&79));
        assert_eq!(slots.iter().flatten().count(), 6);
    }

    #[test]
    /// 마지막 음의 절단 뒤에 표현 불가능한 템포 간격이 남지 않는지 검사합니다.
    fn cutting_cannot_strand_a_tempo_after_the_last_note() {
        let originals = vec![
            n(0, 192, 43),
            n(0, 192, 48),
            n(0, 192, 55),
            n(96, 120, 72),
            n(96, 120, 76),
            n(96, 120, 79),
        ];
        let mut notes = originals.clone();
        let result = schedule(
            &mut notes,
            &[0, 1, 2, 3, 4, 5],
            &Policy {
                originals: &originals,
                probabilities: &[0.9; 6],
                anchors: &[false; 6],
                parts: 3,
                count: 6,
                tempos: &[(0, 120), (123, 140)],
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("rest after final attack")
        );
    }

    #[test]
    /// 분산 반주의 중간 진입이 이어지는 옥타브 선율을 자르거나 보조 트랙으로 밀지 않습니다.
    fn interleaved_accompaniment_preserves_the_ongoing_source_melody() {
        for transpose in [-24, 0, 12] {
            let rows = [
                (0, 48, 96, 0),
                (0, 48, 84, 1),
                (0, 30, 48, 2),
                (0, 30, 36, 3),
                (30, 66, 60, 2),
                (30, 66, 55, 3),
                (48, 96, 91, 0),
                (48, 96, 79, 1),
                (66, 96, 64, 2),
                (66, 96, 60, 3),
            ];
            let mut notes: Vec<_> = rows
                .map(|(on, off, pitch, source)| Note {
                    src: (source, 0),
                    ..n(on, off, pitch + transpose)
                })
                .into();
            let original = notes.clone();
            let slots = run(
                &mut notes,
                &[
                    0.995, 0.949, 0.897, 0.392, 0.99, 0.97, 0.998, 0.938, 0.99, 0.97,
                ],
                3,
                6,
                &[],
            );
            for index in [0, 1, 6, 7] {
                assert_eq!(notes[index], original[index]);
                assert!(slots[..3].iter().any(|part| part.contains(&index)));
            }
            assert!(
                slots[..3]
                    .iter()
                    .any(|part| part.contains(&0) && part.contains(&6))
            );
            assert!(
                slots[..3]
                    .iter()
                    .any(|part| part.contains(&1) && part.contains(&7))
            );
            assert_eq!(slots.iter().flatten().count(), notes.len());
        }
    }

    #[test]
    /// 한 박이 지난 긴 상성부도 같은 원본 선율로 이어지면 아래 반주에 잘리지 않습니다.
    fn sustained_source_melody_outlasts_multiple_lower_attacks() {
        let rows = [
            (0, 24, 77, 0),
            (0, 24, 65, 1),
            (48, 240, 89, 0),
            (48, 240, 77, 1),
            (48, 78, 60, 2),
            (48, 78, 56, 3),
            (78, 114, 60, 2),
            (78, 114, 55, 3),
            (114, 144, 56, 2),
            (114, 144, 51, 3),
            (144, 162, 39, 2),
            (144, 162, 27, 3),
            (240, 288, 87, 0),
            (240, 288, 75, 1),
        ];
        let mut notes: Vec<_> = rows
            .map(|(on, off, pitch, source)| Note {
                src: (source, 0),
                ..n(on, off, pitch)
            })
            .into();
        let original = notes.clone();
        let slots = run(
            &mut notes,
            &[
                0.99, 0.97, 0.998, 0.97, 0.95, 0.90, 0.998, 0.90, 0.998, 0.92, 0.97, 0.85, 0.99,
                0.97,
            ],
            3,
            6,
            &[],
        );
        for index in [2, 3] {
            assert_eq!(notes[index], original[index]);
            assert!(slots[..3].iter().any(|part| part.contains(&index)));
        }
        assert_eq!(slots.iter().flatten().count(), notes.len());
    }

    #[test]
    /// 단지 높거나 판단이 약한 지속음은 새 공격보다 무조건 우선하지 않습니다.
    fn isolated_or_weaker_high_notes_can_still_yield() {
        for (has_neighbor, held_score, new_score) in
            [(false, 0.98, 0.95), (true, 0.50, 0.80), (true, 0.85, 0.98)]
        {
            let mut notes = vec![n(0, 192, 84), n(30, 78, 60)];
            let mut probabilities = vec![held_score, new_score];
            if has_neighbor {
                let mut neighbor = n(192, 240, 86);
                neighbor.src = notes[0].src;
                notes.push(neighbor);
                probabilities.push(held_score);
            }
            let slots = run(&mut notes, &probabilities, 1, 2, &[]);
            assert_eq!(notes[0].off, 30);
            assert!(slots[0].contains(&1));
        }
    }

    #[test]
    /// 다른 파트가 이미 울리고 있으면 선율 보호보다 모든 새 공격의 배치를 우선합니다.
    fn source_lead_preference_does_not_make_attack_placement_infeasible() {
        let mut notes = vec![n(0, 192, 84), n(0, 192, 40), n(30, 78, 60), n(192, 240, 86)];
        notes[3].src = notes[0].src;
        let slots = run(&mut notes, &[0.98, 0.90, 0.95, 0.98], 1, 2, &[]);
        assert_eq!(notes[0].off, 30);
        assert!(slots[0].contains(&2));
        assert_eq!(slots.iter().flatten().count(), notes.len());
    }

    #[test]
    /// 명확한 7화음의 비슷한 후보 중 중복보다 아직 없는 7음을 함께 남깁니다.
    fn clear_seventh_chord_coverage_prefers_an_unrepresented_member() {
        for transpose in [-12, 0, 12] {
            let mut notes: Vec<_> = [36, 48, 52, 55, 58, 64]
                .into_iter()
                .map(|pitch| n(0, 96, pitch + transpose))
                .collect();
            let original = notes.clone();
            let slots = run(&mut notes, &[0.4; 6], 3, 6, &[]);
            let selected: std::collections::BTreeSet<_> = slots[..3]
                .iter()
                .flatten()
                .map(|&index| notes[index].pitch - transpose)
                .collect();
            assert_eq!(selected, [36, 58, 64].into_iter().collect());
            assert_eq!(notes, original);
            assert_eq!(slots.iter().flatten().count(), notes.len());
        }
    }

    #[test]
    /// 탈락한 비화성음도 원래 울리는 동안 판정을 보류하고 끝난 뒤에만 보정을 허용합니다.
    fn removed_nonchord_tones_remain_in_the_original_harmonic_context() {
        for transpose in [-12, 0, 12] {
            for (on, off, active) in [(0, 192, true), (96, 192, true), (0, 96, false)] {
                let mut originals: Vec<_> = [12, 36, 52, 55, 59, 88]
                    .into_iter()
                    .map(|pitch| n(96, 192, pitch + transpose))
                    .collect();
                originals.push(n(on, off, 63 + transpose));
                let mut notes = originals.clone();
                notes[6].off = notes[6].on;
                let before = notes.clone();
                let slots = schedule(
                    &mut notes,
                    &[0, 1, 2, 3, 4, 5],
                    &Policy {
                        originals: &originals,
                        probabilities: &[0.4; 7],
                        anchors: &[false; 7],
                        parts: 3,
                        count: 6,
                        tempos: &[],
                    },
                )
                .unwrap();
                let selected: std::collections::BTreeSet<_> = slots[..3]
                    .iter()
                    .flatten()
                    .map(|&i| notes[i].pitch - transpose)
                    .collect();
                let expected = if active { [12, 36, 52] } else { [12, 59, 88] };
                assert_eq!(selected, expected.into_iter().collect());
                assert_eq!(notes, before);
                let mut assigned: Vec<_> = slots.into_iter().flatten().collect();
                assigned.sort_unstable();
                assert_eq!(assigned, [0, 1, 2, 3, 4, 5]);
            }
        }
    }
}
