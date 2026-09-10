//! 구간 그래프로 핵심 성부와 보조 성부의 용량을 함께 지키며 음표를 고릅니다.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::{Result, ensure};

use crate::Note;

#[derive(Clone)]
struct Edge {
    to: usize,
    reverse: usize,
    capacity: usize,
    cost: i64,
}

/// 잔여 그래프에 정방향과 역방향 간선을 추가합니다.
fn edge(graph: &mut [Vec<Edge>], from: usize, to: usize, capacity: usize, cost: i64) -> usize {
    let index = graph[from].len();
    let reverse = graph[to].len();
    graph[from].push(Edge {
        to,
        reverse,
        capacity,
        cost,
    });
    graph[to].push(Edge {
        to: from,
        reverse: index,
        capacity: 0,
        cost: -cost,
    });
    index
}

/// 최소 비용 흐름으로 음표 길이를 유지하는 핵심 성부 조합을 선택합니다.
pub(crate) fn select(
    notes: &[Note],
    probabilities: &[f64],
    priority: &[bool],
    core_parts: usize,
    total_capacity: usize,
) -> Result<Vec<bool>> {
    ensure!(
        probabilities.len() == notes.len() && priority.len() == notes.len(),
        "Importance count does not match notes"
    );
    ensure!(
        core_parts > 0 && total_capacity >= core_parts,
        "Invalid ensemble capacity"
    );
    if notes.is_empty() {
        return Ok(Vec::new());
    }
    let mut times = Vec::with_capacity(notes.len() * 2);
    for note in notes {
        ensure!(
            note.on >= 0 && note.off > note.on,
            "Invalid interval for core selection"
        );
        times.extend([note.on, note.off]);
    }
    times.sort_unstable();
    times.dedup();
    let count = times.len();
    let mut graph = vec![Vec::new(); count];
    let mut changes = vec![0_i64; count];
    let mut references = Vec::with_capacity(notes.len());
    let mut rewards = Vec::with_capacity(notes.len());
    for (note, &probability) in notes.iter().zip(probabilities) {
        ensure!(probability.is_finite(), "Nonfinite note importance");
        rewards.push(
            (probability.clamp(0.0, 1.0) * note.duration() as f64 * 1024.0)
                .round()
                .clamp(0.0, 1_000_000_000_000.0) as i64,
        );
    }
    // 선율 가산점에도 길이를 곱해 한 음을 여러 공격으로 나눠도 보호 점수가 늘지 않게 합니다.
    for ((reward, note), &anchor) in rewards.iter_mut().zip(notes).zip(priority) {
        if anchor {
            *reward += (0.15 * note.duration() as f64 * 1024.0)
                .round()
                .clamp(0.0, 1_000_000_000_000.0) as i64;
        }
    }
    rewards
        .iter()
        .try_fold(1_i64, |sum, &v| sum.checked_add(v))
        .filter(|&sum| sum < i64::MAX / 8)
        .ok_or_else(|| anyhow::anyhow!("Score is too large for core selection costs"))?;
    for (note, reward) in notes.iter().zip(rewards) {
        let from = times.binary_search(&note.on).unwrap();
        let to = times.binary_search(&note.off).unwrap();
        changes[from] += 1;
        changes[to] -= 1;
        // 지속음과 반복음을 길이로 비교하고 정수 비용을 제한해 잔여 최단 경로의 넘침을 막습니다.
        let index = edge(&mut graph, from, to, 1, -reward);
        references.push((from, index));
    }
    // 핵심 발음 수 + 유휴 흐름 = core_parts이므로 유휴 용량을 제한하면 보조 성부도 용량을 지킵니다.
    let mut active = 0_i64;
    for i in 0..count - 1 {
        active += changes[i];
        ensure!(
            active >= 0 && active as usize <= total_capacity,
            "Input exceeds ensemble capacity at tick {}",
            times[i]
        );
        edge(
            &mut graph,
            i,
            i + 1,
            core_parts.min(total_capacity - active as usize),
            0,
        );
    }

    // 초기 간선은 모두 앞으로 향하므로 DAG 한 번 순회로 음수 비용의 유효 포텐셜을 구합니다.
    let inf = i64::MAX / 4;
    let mut potentials = vec![inf; count];
    potentials[0] = 0;
    for from in 0..count {
        if potentials[from] == inf {
            continue;
        }
        for e in &graph[from] {
            if e.capacity > 0 {
                potentials[e.to] = potentials[e.to].min(potentials[from] + e.cost);
            }
        }
    }
    for value in &mut potentials {
        if *value == inf {
            *value = 0;
        }
    }
    for _ in 0..core_parts {
        let mut distance = vec![inf; count];
        let mut previous = vec![None; count];
        let mut heap = BinaryHeap::new();
        distance[0] = 0;
        heap.push(Reverse((0_i64, 0_usize)));
        while let Some(Reverse((cost, from))) = heap.pop() {
            if distance[from] != cost {
                continue;
            }
            for (index, e) in graph[from].iter().enumerate() {
                if e.capacity == 0 {
                    continue;
                }
                let next = cost + e.cost + potentials[from] - potentials[e.to];
                if next < distance[e.to] {
                    distance[e.to] = next;
                    previous[e.to] = Some((from, index));
                    heap.push(Reverse((next, e.to)));
                }
            }
        }
        ensure!(
            distance[count - 1] != inf,
            "Cannot allocate complete core voices"
        );
        for i in 0..count {
            if distance[i] != inf {
                potentials[i] += distance[i];
            }
        }
        let mut to = count - 1;
        while to != 0 {
            let (from, index) = previous[to].expect("reachable sink has a predecessor");
            let reverse = graph[from][index].reverse;
            graph[from][index].capacity -= 1;
            graph[to][reverse].capacity += 1;
            to = from;
        }
    }
    Ok(references
        .into_iter()
        .map(|(from, index)| graph[from][index].capacity == 0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 구간 선택 검사에 사용할 음표를 만듭니다.
    fn note(on: i64, off: i64) -> Note {
        Note {
            on,
            off,
            pitch: 60,
            vel: 8,
            src: (0, 0),
        }
    }

    /// 핵심 및 보조 성부의 동시발음 수를 확인합니다.
    fn valid(notes: &[Note], selected: &[bool], core: usize, total: usize) -> bool {
        notes.iter().all(|at| {
            let mut counts = [0, 0];
            for (n, &chosen) in notes.iter().zip(selected) {
                if n.on <= at.on && at.on < n.off {
                    counts[usize::from(chosen)] += 1;
                }
            }
            counts[1] <= core && counts[0] <= total - core
        })
    }

    #[test]
    /// 구간 선택이 완전 탐색 최적해와 양쪽 성부 용량을 만족하는지 검사합니다.
    fn selection_matches_exhaustive_optimum_and_bounds_both_ensembles() {
        let mut seed = 13_u64;
        for _ in 0..50 {
            let mut notes = Vec::new();
            let mut probabilities = Vec::new();
            for _ in 0..8 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let on = (seed >> 32) as i64 % 10 * 24;
                let off = on + (1 + (seed >> 48) as i64 % 5) * 24;
                notes.push(note(on, off));
                probabilities.push(((seed >> 40) % 8) as f64 / 8.0);
            }
            let total = notes
                .iter()
                .map(|at| {
                    notes
                        .iter()
                        .filter(|n| n.on <= at.on && at.on < n.off)
                        .count()
                })
                .max()
                .unwrap();
            let core = 2.min(total);
            let selected = select(
                &notes,
                &probabilities,
                &vec![false; notes.len()],
                core,
                total,
            )
            .unwrap();
            assert!(valid(&notes, &selected, core, total));
            let value = |mask: &[bool]| {
                notes
                    .iter()
                    .zip(&probabilities)
                    .zip(mask)
                    .filter(|(_, chosen)| **chosen)
                    .map(|((n, p), _)| n.duration() as f64 * p)
                    .sum::<f64>()
            };
            let best = (0..1 << notes.len())
                .filter_map(|bits| {
                    let mask: Vec<_> = (0..notes.len()).map(|i| bits & (1 << i) != 0).collect();
                    valid(&notes, &mask, core, total).then(|| value(&mask))
                })
                .fold(0.0, f64::max);
            assert_eq!(value(&selected), best);

            let priority: Vec<_> = (0..notes.len()).map(|i| i % 3 == 0).collect();
            let selected = select(&notes, &probabilities, &priority, core, total).unwrap();
            let weighted_value = |mask: &[bool]| {
                (0..notes.len())
                    .filter(|&i| mask[i])
                    .map(|i| {
                        let base =
                            (notes[i].duration() as f64 * probabilities[i] * 1024.0).round() as i64;
                        let prior = if priority[i] {
                            (notes[i].duration() as f64 * 0.15 * 1024.0).round() as i64
                        } else {
                            0
                        };
                        base + prior
                    })
                    .sum::<i64>()
            };
            let best = (0..1 << notes.len())
                .filter_map(|bits| {
                    let mask: Vec<_> = (0..notes.len()).map(|i| bits & (1 << i) != 0).collect();
                    valid(&notes, &mask, core, total).then(|| weighted_value(&mask))
                })
                .max()
                .unwrap();
            assert_eq!(weighted_value(&selected), best);
        }
    }

    #[test]
    /// 충분한 모델 점수 차이가 잘못된 선율 가산점을 이기는지 검사합니다.
    fn learned_evidence_can_override_a_wrong_melody_prior() {
        let notes = [note(0, 192), note(0, 192)];
        assert_eq!(
            select(&notes, &[0.01, 0.99], &[true, false], 1, 2).unwrap(),
            [false, true]
        );
        assert_eq!(
            select(&notes, &[0.5, 0.6], &[true, false], 1, 2).unwrap(),
            [true, false]
        );
    }

    #[test]
    /// 우선 음들이 양립하지 않을 때 길이를 자르지 않고 용량을 지키는지 검사합니다.
    fn incompatible_priority_notes_yield_to_capacity_without_cutting_intervals() {
        let notes = [note(0, 24), note(12, 60), note(36, 84), note(72, 96)];
        let selected = select(
            &notes,
            &[0.2, 0.9, 0.3, 0.8],
            &[true, false, false, true],
            1,
            2,
        )
        .unwrap();
        assert!(valid(&notes, &selected, 1, 2));
        assert_eq!(usize::from(selected[0]) + usize::from(selected[3]), 1);
        assert!(selected[3]);
    }

    #[test]
    /// 한 음의 반복 분할이 핵심 성부 우선도를 바꾸지 않는지 검사합니다.
    fn splitting_an_attack_does_not_change_its_core_priority() {
        let held = [note(0, 192), note(0, 192)];
        assert_eq!(
            select(&held, &[0.8, 0.2], &[false; 2], 1, 2).unwrap(),
            [true, false]
        );
        let mut repeated = vec![note(0, 192)];
        repeated.extend((0..16).map(|i| note(i * 12, (i + 1) * 12)));
        let mut probabilities = vec![0.2; repeated.len()];
        probabilities[0] = 0.8;
        assert_eq!(
            select(
                &repeated,
                &probabilities,
                &vec![false; repeated.len()],
                1,
                2
            )
            .unwrap(),
            std::iter::once(true)
                .chain(std::iter::repeat_n(false, 16))
                .collect::<Vec<_>>()
        );
    }
}
