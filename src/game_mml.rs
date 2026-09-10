//! 현재 내보내기와 분리된 MML 템포 재배치 실험을 제공합니다.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};

use crate::{Note, Tempo, Tick, core};

#[derive(Clone, Debug)]
pub struct GameTrack {
    pub parts: Vec<String>,
    pub tempo_note_splits: usize,
}

/// 템포를 정렬하고 유효한 값과 시작 템포를 확인합니다.
fn canonical_tempos(tempos: &[Tempo], total: Tick) -> Result<Vec<Tempo>> {
    let mut by_tick = BTreeMap::new();
    for &(tick, bpm) in tempos {
        ensure!(tick >= 0 && bpm > 0, "Invalid tempo {bpm} at tick {tick}");
        if tick < total || tick == 0 {
            by_tick.insert(tick, bpm);
        }
    }
    if (total > 0 && by_tick.is_empty())
        || by_tick.first_key_value().is_some_and(|(&tick, _)| tick > 0)
    {
        by_tick.insert(0, 120);
    }
    let mut previous = None;
    Ok(by_tick
        .into_iter()
        .filter(|&(_, bpm)| {
            let changed = previous != Some(bpm);
            previous = Some(bpm);
            changed
        })
        .collect())
}

#[derive(Clone, Copy)]
enum Placement {
    Boundary,
    Rest,
    Held { note_index: usize, remaining: Tick },
}

/// 한 파트에서 템포를 정확히 넣을 수 있는 위치를 찾습니다.
fn placement(notes: &[Note], assigned: &[Tempo], tick: Tick) -> Option<Placement> {
    let next_index = notes.partition_point(|note| note.on < tick);
    if let Some((note_index, note)) = next_index
        .checked_sub(1)
        .map(|index| (index, &notes[index]))
        .filter(|(_, note)| note.off > tick)
    {
        return (core::split_ticks(tick - note.on).is_ok()
            && core::split_ticks(note.off - tick).is_ok())
        .then_some(Placement::Held {
            note_index,
            remaining: note.off - tick,
        });
    }

    // 쉼표 안의 템포는 이전에 배정한 템포까지 포함해 양쪽 길이가 정확히 표현되어야 합니다.
    let prior_note_end = next_index
        .checked_sub(1)
        .map_or(0, |index| notes[index].off);
    let prior_tempo = assigned.last().map_or(0, |tempo| tempo.0);
    if core::split_ticks(tick - prior_note_end.max(prior_tempo)).is_err() {
        return None;
    }
    if let Some(next_note) = notes.get(next_index)
        && core::split_ticks(next_note.on - tick).is_err()
    {
        return None;
    }
    let boundary =
        prior_note_end == tick || notes.get(next_index).is_some_and(|note| note.on == tick);
    Some(if boundary {
        Placement::Boundary
    } else {
        Placement::Rest
    })
}

/// 파트 묶음의 빈 위치에 템포를 배정하고 필요한 음의 꼬리를 처리합니다.
fn assign_group(
    notes: &mut [Vec<Note>],
    assigned: &mut [Vec<Tempo>],
    tempos: &[Tempo],
) -> Result<usize> {
    let mut shortened = 0;
    for &(tick, bpm) in tempos {
        let candidates = notes.iter().zip(assigned.iter()).enumerate().filter_map(
            |(part_index, (notes, assigned))| {
                placement(notes, assigned, tick).map(|place| (part_index, place))
            },
        );
        let (part_index, place) = candidates
            .min_by_key(|&(part_index, place)| match place {
                Placement::Boundary => (0, 0, part_index),
                Placement::Rest => (1, 0, part_index),
                Placement::Held {
                    note_index,
                    remaining,
                } => {
                    let audible = notes[part_index][note_index].vel > 0;
                    (if audible { 3 } else { 2 }, remaining, part_index)
                }
            })
            .with_context(|| {
                format!(
                    "Tempo {bpm} at tick {tick} cannot be placed without rounding musical timing"
                )
            })?;

        if let Placement::Held { note_index, .. } = place {
            // 템포를 가로지르는 타이를 피하려고 발음은 줄이고 남은 시간은 무음으로 유지합니다.
            let mut continuation = notes[part_index][note_index].clone();
            shortened += usize::from(continuation.vel > 0);
            notes[part_index][note_index].off = tick;
            continuation.on = tick;
            continuation.vel = 0;
            notes[part_index].insert(note_index + 1, continuation);
        }
        assigned[part_index].push((tick, bpm));
    }
    Ok(shortened)
}

/// 공격 시점과 파트 수를 유지하면서 게임용 템포 위치를 계산합니다.
pub fn emit_track(parts: &[Vec<Note>], tempos: &[Tempo], total: Tick) -> Result<GameTrack> {
    ensure!(total >= 0, "Negative total duration {total}");
    ensure!(parts.len() <= 4, "The game supports at most four MML parts");
    let tempos = canonical_tempos(tempos, total)?;
    let mut notes = parts.to_vec();
    for (part_index, notes) in notes.iter_mut().enumerate() {
        notes.sort_by_key(|note| note.on);
        // 템포 배치 전에 원본 구간과 음가를 검사해 잘못된 악보를 암묵적으로 보정하지 않습니다.
        core::emit_part(notes, &[], total)
            .with_context(|| format!("Game MML part {}", part_index + 1))?;
    }
    let mut assigned = vec![Vec::new(); notes.len()];
    let instrumental_count = notes.len().min(3);
    let mut tempo_note_splits = 0;
    if instrumental_count > 0 {
        tempo_note_splits += assign_group(
            &mut notes[..instrumental_count],
            &mut assigned[..instrumental_count],
            &tempos,
        )?;
    }
    // 노래용 네 번째 파트는 악기용 세 파트와 별개로 템포를 적용합니다.
    if notes.len() == 4 && !notes[3].is_empty() {
        tempo_note_splits += assign_group(&mut notes[3..], &mut assigned[3..], &tempos)?;
    }
    let parts = notes
        .iter()
        .zip(assigned.iter())
        .enumerate()
        .map(|(index, (notes, tempos))| {
            core::emit_part(notes, tempos, total)
                .with_context(|| format!("Game MML part {}", index + 1))
        })
        .collect::<Result<_>>()?;
    Ok(GameTrack {
        parts,
        tempo_note_splits,
    })
}
