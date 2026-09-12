//! 모바일 MML의 음가 제약을 검사하고 필요한 공유 시간 경계만 최대 3틱 보정합니다.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::OnceLock;

use anyhow::{Context, Result, ensure};

use crate::{Score, Tick, core};

/// 모바일에서 사용하는 서로 다른 음가 세 개 이하의 합을 준비합니다.
fn inverse_table() -> &'static [bool; 768] {
    static TABLE: OnceLock<[bool; 768]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let atoms = [
            384, 576, 192, 288, 96, 144, 48, 72, 24, 36, 12, 18, 6, 9, 64, 32, 16, 8,
        ];
        let mut table = [false; 768];
        for (i, &a) in atoms.iter().enumerate() {
            table[a] = true;
            for (j, &b) in atoms.iter().enumerate().skip(i + 1) {
                if a + b < table.len() {
                    table[a + b] = true;
                }
                for &c in atoms.iter().skip(j + 1) {
                    if a + b + c < table.len() {
                        table[a + b + c] = true;
                    }
                }
            }
        }
        table
    })
}

/// MabiIcco의 모바일 역변환 표와 잔여 틱 분해로 출력 가능 여부를 검사합니다.
/// 참고: https://github.com/fourthline/mmlTools/blob/master/src/jp/fourthline/mmlTools/core/MMLTicks.java
pub(crate) fn compatible_duration(mut ticks: Tick) -> bool {
    if ticks < 0 {
        return false;
    }
    if ticks > 768 {
        ticks -= ((ticks - 769) / 576 + 1) * 576;
    }
    let table = inverse_table();
    for base in [384, 192, 96, 48, 24, 12, 6] {
        if table.get(ticks as usize).copied().unwrap_or(false) {
            return true;
        }
        ticks %= base;
    }
    ticks == 0
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    timed: bool,
}

impl Span {
    /// 음가의 출력 가능 여부 또는 인접 경계의 시간 순서를 확인합니다.
    fn valid(&self, points: &[Tick]) -> bool {
        let duration = points[self.end] - points[self.start];
        if self.timed {
            duration >= core::MIN_TICK && compatible_duration(duration)
        } else {
            duration >= 0
        }
    }
}

#[derive(Default)]
struct Boundaries {
    indices: BTreeMap<Tick, usize>,
    original: Vec<Tick>,
    spans: Vec<Span>,
    pairs: BTreeSet<(usize, usize)>,
}

impl Boundaries {
    /// 같은 원본 시점에 언제나 같은 경계 번호를 부여합니다.
    fn index(&mut self, tick: Tick) -> usize {
        let next = self.original.len();
        let index = *self.indices.entry(tick).or_insert(next);
        if index == next {
            self.original.push(tick);
        }
        index
    }

    /// 양수 구간을 공유 경계에 연결하고 중복 검사를 생략합니다.
    fn add(&mut self, start: Tick, end: Tick) -> Result<()> {
        ensure!(
            start >= 0 && end >= start,
            "잘못된 시간 구간: {start}~{end}"
        );
        if start < end {
            let pair = (self.index(start), self.index(end));
            if self.pairs.insert(pair) {
                self.spans.push(Span {
                    start: pair.0,
                    end: pair.1,
                    timed: true,
                });
            }
        }
        Ok(())
    }

    /// 서로 다른 파트와 박자표의 이웃 경계가 이동 후 순서를 바꾸지 못하게 연결합니다.
    fn preserve_order(&mut self) {
        let ordered: Vec<_> = self.indices.values().copied().collect();
        for pair in ordered.windows(2) {
            if !self.pairs.contains(&(pair[0], pair[1])) {
                self.spans.push(Span {
                    start: pair[0],
                    end: pair[1],
                    timed: false,
                });
            }
        }
    }

    /// 불가능한 구간에서 시작해 연결된 구간만 다시 검사하며 경계 이동을 전파합니다.
    fn resolve(&self) -> Result<BTreeMap<Tick, Tick>> {
        let mut mapped = self.original.clone();
        let mut grid = vec![0; mapped.len()];
        let mut adjacent = vec![Vec::new(); mapped.len()];
        for (i, span) in self.spans.iter().enumerate() {
            adjacent[span.start].push(i);
            adjacent[span.end].push(i);
        }
        let mut queue: VecDeque<_> = (0..self.spans.len()).collect();
        let mut queued = vec![true; self.spans.len()];
        while let Some(i) = queue.pop_front() {
            queued[i] = false;
            let span = self.spans[i];
            if span.valid(&mapped) {
                continue;
            }
            let target = if span.timed && (grid[span.start] < 3 || grid[span.end] < 3) {
                3
            } else {
                6
            };
            let mut changed = false;
            for endpoint in [span.start, span.end] {
                if grid[endpoint] >= target {
                    continue;
                }
                let tick = self.original[endpoint];
                let remainder = tick % target;
                let lower = tick - remainder;
                let nearest = if remainder * 2 < target {
                    lower
                } else {
                    lower
                        .checked_add(target)
                        .context("시간 경계가 너무 큽니다")?
                };
                grid[endpoint] = target;
                mapped[endpoint] = nearest;
                changed = true;
                for &neighbor in &adjacent[endpoint] {
                    if !queued[neighbor] {
                        queued[neighbor] = true;
                        queue.push_back(neighbor);
                    }
                }
            }
            ensure!(
                changed,
                "모바일 내보내기: {}~{}틱 구간을 3틱 이내에서 변환할 수 없습니다",
                self.original[span.start],
                self.original[span.end]
            );
        }
        Ok(self
            .original
            .iter()
            .zip(mapped)
            .filter_map(|(&from, to)| (from != to).then_some((from, to)))
            .collect())
    }
}

/// 음표·쉼표와 멜로디의 템포 분할 구간을 공유 경계 그래프로 만듭니다.
fn boundaries(score: &Score) -> Result<Boundaries> {
    let mut boundaries = Boundaries::default();
    boundaries.index(0);
    let tempos = score.tempos();
    for (ti, track) in score.tracks.iter().enumerate() {
        for (pi, part) in track.parts.iter().enumerate() {
            let mut ordered: Vec<_> = part.iter().collect();
            ordered.sort_by_key(|note| note.on);
            let mut previous = 0;
            let mut carrier = BTreeSet::from([0]);
            for note in ordered {
                ensure!(
                    note.on >= previous && note.off > note.on,
                    "트랙 {} · 파트 {}의 음표 구간이 겹치거나 잘못되었습니다",
                    ti + 1,
                    pi + 1
                );
                boundaries.add(previous, note.on)?;
                boundaries.add(note.on, note.off)?;
                if pi == 0 {
                    carrier.insert(note.on);
                    carrier.insert(note.off);
                }
                previous = note.off;
            }
            if pi == 0 {
                if let Some(&(last, _)) = tempos.last()
                    && last > previous
                {
                    boundaries.add(previous, last)?;
                }
                carrier.extend(tempos.iter().map(|&(tick, _)| tick));
                let mut points = carrier.into_iter();
                let mut previous = points.next().unwrap_or(0);
                for tick in points {
                    boundaries.add(previous, tick)?;
                    previous = tick;
                }
            }
        }
    }
    let mut signature = false;
    for line in &score.tail {
        if line.starts_with('[') {
            signature = line == "[time-signature]";
        } else if signature
            && let Some((tick, _)) = line.split_once('=')
            && let Ok(tick) = tick.parse::<Tick>()
            && tick >= 0
        {
            boundaries.index(tick);
        }
    }
    boundaries.preserve_order();
    Ok(boundaries)
}

/// 음표·쉼표·멜로디 템포 구간을 변경하지 않고 모바일 출력 가능 여부를 검사합니다.
pub(crate) fn score_is_compatible(score: &Score) -> bool {
    boundaries(score).is_ok_and(|boundaries| {
        boundaries
            .spans
            .iter()
            .all(|span| span.valid(&boundaries.original))
    })
}

/// 원본을 유지하며 모바일로 내보낼 수 없는 구간의 공유 경계만 보정합니다.
pub(crate) fn prepare_score(score: &Score) -> Result<(Score, usize)> {
    let mapping = boundaries(score)?.resolve()?;
    if mapping.is_empty() {
        return Ok((score.clone(), 0));
    }
    let map = |tick: Tick| mapping.get(&tick).copied().unwrap_or(tick);
    let mut output = score.clone();
    let tempos: Vec<_> = score
        .tempos()
        .into_iter()
        .map(|(t, v)| (map(t), v))
        .collect();
    for line in &mut output.head {
        if line.starts_with("tempo=") {
            *line = format!(
                "tempo={}",
                tempos
                    .iter()
                    .map(|(t, v)| format!("{t}T{v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
    let mut signature = false;
    for line in &mut output.tail {
        if line.starts_with('[') {
            signature = line == "[time-signature]";
        } else if signature
            && let Some((tick, value)) = line.split_once('=')
            && let Ok(tick) = tick.parse::<Tick>()
            && mapping.contains_key(&tick)
        {
            *line = format!("{}={value}", map(tick));
        }
    }
    for track in &mut output.tracks {
        let mut changed = false;
        for part in &mut track.parts {
            for note in part {
                let on = map(note.on);
                let off = map(note.off);
                changed |= on != note.on || off != note.off;
                note.on = on;
                note.off = off;
            }
        }
        for (tick, _) in &mut track.tempos {
            let moved = map(*tick);
            changed |= moved != *tick;
            *tick = moved;
        }
        if changed {
            let total = track
                .parts
                .iter()
                .flatten()
                .map(|note| note.off)
                .max()
                .unwrap_or(0);
            let parts: Vec<_> = track
                .parts
                .iter()
                .enumerate()
                .map(|(pi, notes)| {
                    core::emit_part(notes, if pi == 0 { &track.tempos } else { &[] }, total)
                })
                .collect::<Result<_>>()?;
            track.mml = format!("MML@{};", parts.join(","));
        }
    }
    ensure!(
        score_is_compatible(&output),
        "모바일 시간 보정 검증에 실패했습니다"
    );
    Ok((output, mapping.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Note, Tempo, Track};

    /// 음표·템포와 MML 표현이 일치하는 검사 악보를 만듭니다.
    fn score(parts: &[&[(Tick, Tick, i32)]], tempos: &[Tempo]) -> Score {
        let parts: Vec<Vec<_>> = parts
            .iter()
            .enumerate()
            .map(|(pi, notes)| {
                notes
                    .iter()
                    .map(|&(on, off, pitch)| Note {
                        on,
                        off,
                        pitch,
                        vel: 11,
                        src: (0, pi),
                    })
                    .collect()
            })
            .collect();
        let mml = parts
            .iter()
            .enumerate()
            .map(|(pi, notes)| {
                core::emit_part(notes, if pi == 0 { tempos } else { &[] }, 0).unwrap()
            })
            .collect::<Vec<_>>();
        Score {
            head: vec![
                "[mml-score]".into(),
                format!(
                    "tempo={}",
                    tempos
                        .iter()
                        .map(|(t, v)| format!("{t}T{v}"))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ],
            tracks: vec![Track {
                mml: format!("MML@{};", mml.join(",")),
                meta: BTreeMap::from([
                    ("name".into(), "검사 악보".into()),
                    ("visible".into(), "true".into()),
                ]),
                parts,
                tempos: tempos.to_vec(),
            }],
            tail: vec!["[time-signature]".into(), "0=4/4".into()],
        }
    }

    #[test]
    /// 공식 모바일 역표의 빈 항목과 탐욕 분해가 성공하는 항목을 구분합니다.
    fn mobile_inverse_and_remainder_examples_match_upstream() {
        assert_eq!(inverse_table().iter().filter(|&&value| value).count(), 397);
        for ticks in [0, 6, 8, 9, 14, 17, 18, 20, 71, 83, 95, 114, 116, 768] {
            assert!(compatible_duration(ticks), "{ticks}");
        }
        for ticks in [
            -1, 1, 2, 3, 4, 5, 7, 10, 11, 13, 19, 55, 67, 101, 103, 107, 115, 193, 195, 385, 386,
            387, 579, 769,
        ] {
            assert!(!compatible_duration(ticks), "{ticks}");
        }
        for ticks in (6..=60_000).step_by(6) {
            assert!(compatible_duration(ticks), "{ticks}");
        }
    }

    #[test]
    /// 이미 호환되는 비격자 음표와 원래 MML 문자열을 그대로 유지합니다.
    fn compatible_score_keeps_exact_mml_and_timing() {
        let mut original = score(&[&[(0, 17, 60), (31, 102, 62)]], &[(0, 120)]);
        original.tracks[0].mml = "MML@t120v11c22r27d8&d64&d64.&d48;".into();
        let before = core::serialize_mmi(&original);
        assert_eq!(
            core::parse_mmi(&before).unwrap().all_notes(),
            original.all_notes()
        );
        assert!(score_is_compatible(&original));
        let (prepared, changes) = prepare_score(&original).unwrap();
        assert_eq!(changes, 0);
        assert_eq!(core::serialize_mmi(&prepared), before);
        assert_eq!(prepared.all_notes(), original.all_notes());
    }

    #[test]
    /// 한 경계 이동이 연결된 쉼표·음표에만 전파되고 모든 트랙이 같은 시점을 공유합니다.
    fn shared_boundary_changes_propagate_without_global_quantization() {
        let mut original = score(
            &[&[(0, 19, 60), (31, 49, 62), (1008, 1032, 64)]],
            &[(0, 120)],
        );
        let mut other = score(&[&[(19, 43, 67)]], &[(0, 120)]).tracks.remove(0);
        other.parts[0][0].src = (1, 0);
        original.tracks.push(other);
        let before = core::serialize_mmi(&original);
        let (prepared, changes) = prepare_score(&original).unwrap();
        assert_eq!(changes, 3);
        assert_eq!(prepared.tracks[0].parts[0][0].off, 18);
        assert_eq!(
            (
                prepared.tracks[0].parts[0][1].on,
                prepared.tracks[0].parts[0][1].off
            ),
            (30, 48)
        );
        assert_eq!(prepared.tracks[1].parts[0][0].on, 18);
        assert_eq!(prepared.tracks[1].parts[0][0].off, 43);
        assert_eq!(
            prepared.tracks[0].parts[0][2],
            original.tracks[0].parts[0][2]
        );
        assert_eq!(core::serialize_mmi(&original), before);
        assert!(score_is_compatible(&prepared));
        for (old, new) in original.all_notes().iter().zip(prepared.all_notes()) {
            assert!((old.on - new.on).abs() <= 1 && (old.off - new.off).abs() <= 1);
            assert_eq!((old.pitch, old.vel, old.src), (new.pitch, new.vel, new.src));
        }
        let reparsed = core::parse_mmi(&core::serialize_mmi(&prepared)).unwrap();
        assert_eq!(reparsed.all_notes(), prepared.all_notes());
        let (again, changes) = prepare_score(&prepared).unwrap();
        assert_eq!(changes, 0);
        assert_eq!(core::serialize_mmi(&again), core::serialize_mmi(&prepared));
    }

    #[test]
    /// 템포와 박자표가 음표와 같은 경계 이동을 적용받는지 검사합니다.
    fn tempo_and_meter_follow_shared_adjusted_boundaries() {
        let mut original = score(&[&[(0, 115, 60)], &[(19, 43, 67)]], &[(0, 120), (19, 144)]);
        original.tail.push("19=3/4".into());
        original.tail.extend(["[marker]".into(), "19=표시".into()]);
        let (prepared, changes) = prepare_score(&original).unwrap();
        assert_eq!(changes, 2);
        assert_eq!(prepared.tempos(), [(0, 120), (18, 144)]);
        assert_eq!(prepared.tracks[0].parts[0][0].off, 114);
        assert_eq!(prepared.tracks[0].parts[1][0].on, 18);
        assert!(prepared.head.contains(&"tempo=0T120,18T144".into()));
        assert!(prepared.tail.contains(&"18=3/4".into()));
        assert!(prepared.tail.contains(&"19=표시".into()));
        let reparsed = core::parse_mmi(&core::serialize_mmi(&prepared)).unwrap();
        assert_eq!(reparsed.tempos(), prepared.tempos());
        assert_eq!(reparsed.all_notes(), prepared.all_notes());
    }

    #[test]
    /// MML 본문에 없는 공통 템포도 앞부분 메타데이터에서 읽어 같은 경계로 옮깁니다.
    fn header_only_tempos_remain_complete_after_mml_regeneration() {
        let mut original = score(&[&[(0, 115, 60)]], &[]);
        original.head[1] = "tempo=0T120,19T144".into();
        original
            .tracks
            .push(score(&[&[(0, 115, 67)]], &[]).tracks.remove(0));
        original.tracks[1].parts[0][0].src = (1, 0);
        let before = core::parse_mmi(&core::serialize_mmi(&original)).unwrap();
        assert_eq!(before.all_notes(), original.all_notes());
        assert_eq!(before.tempos(), original.tempos());
        let (prepared, _) = prepare_score(&original).unwrap();
        let reparsed = core::parse_mmi(&core::serialize_mmi(&prepared)).unwrap();
        assert_eq!(reparsed.all_notes(), prepared.all_notes());
        assert_eq!(reparsed.tempos(), [(0, 120), (18, 144)]);
        assert!(prepared.tracks.iter().all(|track| track.tempos.is_empty()));
        assert!(score_is_compatible(&reparsed));
    }

    #[test]
    /// 멜로디 외의 파트는 외부 편집기가 다른 곳에 둘 수 있는 템포로 강제 분할하지 않습니다.
    fn tempo_subdivision_only_constrains_the_actual_carrier() {
        let original = score(&[&[(0, 24, 60)], &[(9, 33, 67)]], &[(0, 120), (12, 144)]);
        assert!(score_is_compatible(&original));
        assert_eq!(prepare_score(&original).unwrap().1, 0);
    }

    #[test]
    /// 최대 이동 범위에서 해결할 수 없는 짧은 구간은 음표를 삭제하지 않고 실패합니다.
    fn impossible_short_span_returns_an_error_without_mutating_the_score() {
        let mut original = score(&[&[(0, 12, 60)]], &[(0, 120)]);
        original.tracks[0].parts[0][0].off = 2;
        let before = original.all_notes();
        assert!(!score_is_compatible(&original));
        assert!(prepare_score(&original).is_err());
        assert_eq!(original.all_notes(), before);
    }

    #[test]
    /// 3틱 격자로 해결되지 않는 긴 구간도 국소 6틱 격자 승격으로 출력합니다.
    fn difficult_long_durations_promote_only_the_required_boundaries() {
        for ticks in [195, 386, 387, 579, 962] {
            let original = score(&[&[(0, ticks, 60)]], &[(0, 120)]);
            let (prepared, changes) = prepare_score(&original).unwrap();
            assert_eq!(changes, 1);
            let note = &prepared.tracks[0].parts[0][0];
            assert_eq!(note.on, 0);
            assert!((note.off - ticks).abs() <= 3);
            assert!(score_is_compatible(&prepared));
            assert_eq!(prepared.all_notes().len(), original.all_notes().len());
            assert_eq!((note.pitch, note.vel, note.src), (60, 11, (0, 0)));
        }
    }

    #[test]
    /// 더 큰 격자 이동이 다른 파트와 박자표의 이웃 경계를 추월하지 않게 합니다.
    fn promotion_preserves_cross_part_and_meter_order() {
        let mut original = score(&[&[(0, 195, 60)], &[(196, 220, 67)]], &[(0, 120)]);
        original.tail.push("196=3/4".into());
        let (prepared, _) = prepare_score(&original).unwrap();
        assert_eq!(prepared.tracks[0].parts[0][0].off, 198);
        assert_eq!(prepared.tracks[0].parts[1][0].on, 198);
        assert_eq!(prepared.tracks[0].parts[1][0].off, 220);
        assert!(prepared.tail.contains(&"198=3/4".into()));
        assert!(score_is_compatible(&prepared));
        assert_eq!(prepare_score(&prepared).unwrap().1, 0);
    }

    #[test]
    /// 마지막 음표 뒤 여러 템포에 걸친 쉼표의 전체 길이도 함께 검사합니다.
    fn trailing_carrier_rest_checks_the_whole_span_and_tempo_fragments() {
        let original = score(
            &[&[(0, 6, 60)], &[(0, 384, 67)]],
            &[(0, 120), (70, 130), (106, 140), (115, 150), (121, 160)],
        );
        assert!(!score_is_compatible(&original));
        let (prepared, changes) = prepare_score(&original).unwrap();
        assert!(changes > 0);
        assert!(score_is_compatible(&prepared));
        let reparsed = core::parse_mmi(&core::serialize_mmi(&prepared)).unwrap();
        assert_eq!(reparsed.tempos(), prepared.tempos());
        assert_eq!(reparsed.all_notes(), prepared.all_notes());
    }
}
