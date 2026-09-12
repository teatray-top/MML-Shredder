//! 모든 파트를 공통 시간에서 글자 수 한도에 맞게 분할합니다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use crate::core::{MIN_TICK, emit_part, track_from_mml};
use crate::{Note, Score, Tempo, Tick};

#[derive(Clone, Debug)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub struct SplitOptions {
    pub limit: usize,
    pub min_gap: Tick,
    pub big_gap: Tick,
}

impl Default for SplitOptions {
    /// 기본 분할 설정을 만듭니다.
    fn default() -> Self {
        Self {
            limit: 2400,
            min_gap: 24,
            big_gap: 96,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScrollChunk {
    pub score: Score,
    pub mml_parts: Vec<Vec<String>>,
    pub game_parts: Vec<Vec<String>>,
    pub tempo_note_splits: usize,
    pub start: Tick,
    pub end: Tick,
    pub widths: Vec<usize>,
    gap: Tick,
}

#[derive(Clone, Debug)]
pub struct SplitResult {
    pub chunks: Vec<ScrollChunk>,
    pub report: String,
}

/// 악보의 템포를 정리하고 시작 템포를 보완합니다.
fn canonical_tempos(score: &Score) -> Vec<Tempo> {
    score
        .tempos()
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect()
}

/// 지정한 시점에서 적용되는 템포를 구합니다.
fn tempo_at(tempos: &[Tempo], tick: Tick) -> i32 {
    tempos
        .iter()
        .rev()
        .find(|(t, _)| *t <= tick)
        .map_or(120, |(_, v)| *v)
}

/// 구간 안의 템포를 구간 시작 기준으로 옮깁니다.
fn local_tempos(tempos: &[Tempo], a: Tick, b: Tick) -> Vec<Tempo> {
    std::iter::once((0, tempo_at(tempos, a)))
        .chain(
            tempos
                .iter()
                .filter(|(t, _)| a < *t && *t < b)
                .map(|(t, v)| (t - a, *v)),
        )
        .collect()
}

/// 구간과 겹치는 음표를 잘라 구간 시작 기준으로 옮깁니다.
fn slice_notes(notes: &[Note], a: Tick, b: Tick) -> Vec<Note> {
    notes
        .iter()
        .filter_map(|n| {
            let on = n.on.max(a);
            let off = n.off.min(b);
            (on < off).then(|| Note {
                on: on - a,
                off: off - a,
                ..n.clone()
            })
        })
        .collect()
}

/// 음표 조각을 생략하지 않고 구간의 모든 파트를 인코딩합니다.
fn chunk_text(score: &Score, tempos: &[Tempo], a: Tick, b: Tick) -> Result<Vec<Vec<String>>> {
    encode_chunk_parts(score, tempos, a, b, false)
}

/// 파트별 구간 인코딩 결과를 원본 순서로 모읍니다.
fn encode_chunk_parts(
    score: &Score,
    tempos: &[Tempo],
    a: Tick,
    b: Tick,
    drop_short: bool,
) -> Result<Vec<Vec<String>>> {
    let local = local_tempos(tempos, a, b);
    let jobs: Vec<_> = score
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, track)| {
            track
                .parts
                .iter()
                .enumerate()
                .map(move |(pi, notes)| (ti, pi, notes.as_slice()))
        })
        .collect();
    // 구간 길이와 무관하게 원본 파트를 훑으므로 원본 음표 수로 작업량을 계산합니다.
    let work = jobs.iter().map(|(_, _, notes)| notes.len()).sum();
    let results = crate::parallel::map(&jobs, work, |&(ti, pi, notes)| {
        let mut notes = slice_notes(notes, a, b);
        if drop_short {
            notes.retain(|note| note.duration() >= MIN_TICK);
        }
        let output = emit_part(&notes, if pi == 0 { &local } else { &[] }, b - a);
        if drop_short {
            output
        } else {
            output.with_context(|| format!("track {}, part {}, ticks {a}..{b}", ti + 1, pi + 1))
        }
    });
    // 완료 순서와 무관하게 첫 오류도 원본 파트 순서를 따릅니다.
    let texts = results.into_iter().collect::<Result<Vec<_>>>()?;
    let mut texts = texts.into_iter();
    Ok(score
        .tracks
        .iter()
        .map(|track| texts.by_ref().take(track.parts.len()).collect())
        .collect())
}

/// 트랙별 파트 문자열의 길이를 펼칩니다.
fn text_widths(texts: &[Vec<String>]) -> Vec<usize> {
    texts.iter().flatten().map(|s| s.chars().count()).collect()
}

/// 구간의 모든 파트가 글자 수 한도 안에 들어가는지 확인합니다.
fn fits(
    score: &Score,
    tempos: &[Tempo],
    a: Tick,
    b: Tick,
    limit: usize,
    mobile: bool,
) -> Result<bool> {
    if mobile && !mobile_chunk_compatible(score, tempos, a, b) {
        return Ok(false);
    }
    Ok(text_widths(&chunk_text(score, tempos, a, b)?)
        .into_iter()
        .all(|width| width <= limit))
}

/// 모든 음이 쉬는 구간 중 최소 길이 이상인 구간을 찾습니다.
fn gaps(notes: &[Note], min_len: Tick) -> Vec<(Tick, Tick)> {
    let mut intervals: Vec<_> = notes.iter().map(|n| (n.on, n.off)).collect();
    intervals.sort_unstable();
    let mut end = 0;
    let mut result = Vec::new();
    for (on, off) in intervals {
        if on > end && on - end >= min_len {
            result.push((end, on - end));
        }
        end = end.max(off);
    }
    result
}

struct CutRange {
    start: Tick,
    end: Tick,
    prior: Tick,
    period: Tick,
    offsets: Vec<Tick>,
}

impl CutRange {
    /// 지정한 틱까지 포함되는 유효 지점 수를 계산합니다.
    fn count_through(&self, tick: Tick) -> Tick {
        let length = (tick.min(self.end) - self.start + 1).max(0);
        length / self.period * self.offsets.len() as Tick
            + self
                .offsets
                .partition_point(|offset| *offset < length % self.period) as Tick
    }
}

// 긴 악보의 모든 틱을 나열하지 않고 유효 범위와 반복 패턴의 누적 순번만 저장합니다.
struct LegalCuts {
    ranges: Vec<CutRange>,
    count: Tick,
    mobile: bool,
}

impl LegalCuts {
    /// 금지 구간을 제외한 분할 지점을 연속 범위로 정리합니다.
    fn new(mut forbidden: Vec<(Tick, Tick)>, total: Tick) -> Self {
        forbidden.retain(|(a, b)| a <= b && *b >= 1 && *a < total);
        forbidden.sort_unstable();
        let mut result = Self {
            ranges: Vec::new(),
            count: 0,
            mobile: false,
        };
        let mut next = 1;
        for (a, b) in forbidden {
            let a = a.max(1);
            let b = b.min(total - 1);
            if next < a {
                result.push(next, a - 1, 1, vec![0]);
            }
            next = next.max(b + 1);
        }
        if next <= total {
            result.push(next, total, 1, vec![0]);
        }
        result
    }

    /// 반복되는 허용 지점 패턴과 누적 순번을 추가합니다.
    fn push(&mut self, start: Tick, end: Tick, period: Tick, offsets: Vec<Tick>) {
        if offsets.is_empty() {
            return;
        }
        let range = CutRange {
            start,
            end,
            prior: self.count,
            period,
            offsets,
        };
        self.count += range.count_through(end);
        self.ranges.push(range);
    }

    /// 유효 지점의 순번을 실제 틱으로 변환합니다.
    fn tick(&self, rank: Tick) -> Tick {
        let i = self.ranges.partition_point(|range| range.prior <= rank) - 1;
        let range = &self.ranges[i];
        let local = rank - range.prior;
        range.start
            + local / range.offsets.len() as Tick * range.period
            + range.offsets[(local % range.offsets.len() as Tick) as usize]
    }

    /// 지정한 틱 다음에 오는 첫 유효 지점의 순번을 구합니다.
    fn first_after(&self, tick: Tick) -> Tick {
        let i = self.ranges.partition_point(|range| range.end <= tick);
        match self.ranges.get(i) {
            Some(range) => range.prior + range.count_through(tick),
            None => self.count,
        }
    }

    /// 지정한 틱이 유효한 분할 지점인지 확인합니다.
    fn contains(&self, tick: Tick) -> bool {
        let i = self.ranges.partition_point(|range| range.end < tick);
        self.ranges.get(i).is_some_and(|range| {
            range.start <= tick
                && tick <= range.end
                && range
                    .offsets
                    .binary_search(&((tick - range.start) % range.period))
                    .is_ok()
        })
    }
}

/// 표현 불가능한 짧은 조각이 생기는 분할 구간을 제외합니다.
fn forbidden_segment(out: &mut Vec<(Tick, Tick)>, start: Tick, end: Tick, sounding: bool) {
    if sounding {
        out.push((start + 1, (start + MIN_TICK - 1).min(end - 1)));
    }
    // 경계 직전 절단은 다음 장에 1~5틱 음표·쉼표·템포 조각을 만들 수 있습니다.
    out.push(((end - MIN_TICK + 1).max(start + 1), end - 1));
}

/// 템포로 나뉜 각 구간의 금지 분할 지점을 추가합니다.
fn add_segments(
    out: &mut Vec<(Tick, Tick)>,
    start: Tick,
    end: Tick,
    sounding: bool,
    tempos: &[Tempo],
) {
    let mut previous = start;
    for &(tick, _) in tempos.iter().filter(|(t, _)| start < *t && *t < end) {
        forbidden_segment(out, previous, tick, sounding);
        previous = tick;
    }
    forbidden_segment(out, previous, end, sounding);
}

/// 음표·쉼표·템포의 길이를 보존할 수 있는 분할 지점을 구합니다.
fn legal_cuts(score: &Score, tempos: &[Tempo], total: Tick) -> LegalCuts {
    let mut forbidden = Vec::new();
    // 장 끝의 쉼표는 생략되므로 전체 무음은 다음 장에 온전히 남기도록 시작점에서 자릅니다.
    for (start, length) in gaps(&score.all_notes(), 1) {
        forbidden.push((start + 1, start + length));
    }
    for track in &score.tracks {
        for (pi, notes) in track.parts.iter().enumerate() {
            let tp = if pi == 0 { tempos } else { &[] };
            let mut ordered: Vec<_> = notes.iter().collect();
            ordered.sort_by_key(|n| n.on);
            let mut previous = 0;
            for note in ordered {
                add_segments(&mut forbidden, previous, note.on, false, tp);
                add_segments(&mut forbidden, note.on, note.off, true, tp);
                previous = note.off;
            }
            if let Some(&(last, _)) = tp.iter().rev().find(|(t, _)| *t < total && *t > previous) {
                add_segments(&mut forbidden, previous, last, false, tp);
            }
        }
    }
    LegalCuts::new(forbidden, total)
}

/// 모바일에서 전체 음가와 템포로 나뉜 조각을 모두 표현할 수 있는지 확인합니다.
fn mobile_span_compatible(start: Tick, end: Tick, tempos: &[Tempo]) -> bool {
    if !crate::mobile::compatible_duration(end - start) {
        return false;
    }
    let mut previous = start;
    for &(tick, _) in tempos
        .iter()
        .filter(|(tick, _)| start < *tick && *tick < end)
    {
        if !crate::mobile::compatible_duration(tick - previous) {
            return false;
        }
        previous = tick;
    }
    crate::mobile::compatible_duration(end - previous)
}

/// 장의 시작과 끝을 함께 적용한 음표·쉼표·템포 조각을 검사합니다.
fn mobile_chunk_compatible(score: &Score, tempos: &[Tempo], a: Tick, b: Tick) -> bool {
    for track in &score.tracks {
        for (pi, notes) in track.parts.iter().enumerate() {
            let tp = if pi == 0 { tempos } else { &[] };
            let mut previous = a;
            for note in notes.iter().filter(|note| note.off > a && note.on < b) {
                let on = note.on.max(a);
                let off = note.off.min(b);
                if !mobile_span_compatible(previous, on, tp) || !mobile_span_compatible(on, off, tp)
                {
                    return false;
                }
                previous = off;
            }
            if let Some(&(last, _)) = tp
                .iter()
                .rev()
                .find(|(tick, _)| previous < *tick && *tick < b)
                && !mobile_span_compatible(previous, last, tp)
            {
                return false;
            }
        }
    }
    true
}

/// 모바일 절단점 검사에 필요한 원래 음가와 템포 조각을 모읍니다.
fn mobile_spans(score: &Score, tempos: &[Tempo], total: Tick) -> Vec<(Tick, Tick)> {
    let mut spans = Vec::new();
    let mut add = |start: Tick, end: Tick, tp: &[Tempo]| {
        if end <= start {
            return;
        }
        spans.push((start, end));
        let mut previous = start;
        for &(tick, _) in tp.iter().filter(|(tick, _)| start < *tick && *tick < end) {
            spans.push((previous, tick));
            previous = tick;
        }
        if previous != start {
            spans.push((previous, end));
        }
    };
    for track in &score.tracks {
        for (pi, notes) in track.parts.iter().enumerate() {
            let tp = if pi == 0 { tempos } else { &[] };
            let mut previous = 0;
            for note in notes {
                add(previous, note.on, tp);
                add(note.on, note.off, tp);
                previous = note.off;
            }
            if let Some(&(last, _)) = tp
                .iter()
                .rev()
                .find(|(tick, _)| previous < *tick && *tick < total)
            {
                add(previous, last, tp);
            }
        }
    }
    spans.sort_unstable();
    spans.dedup();
    spans
}

/// 긴 구간은 576틱 반복 패턴으로 저장하며 모바일에 안전한 공통 절단점을 고릅니다.
fn mobile_legal_cuts(score: &Score, tempos: &[Tempo], total: Tick) -> LegalCuts {
    let base = legal_cuts(score, tempos, total);
    let spans = mobile_spans(score, tempos, total);
    let mut changes: BTreeMap<Tick, Vec<(usize, bool)>> = BTreeMap::new();
    changes.entry(1).or_default();
    changes.entry(total + 1).or_default();
    for range in &base.ranges {
        changes.entry(range.start).or_default();
        changes.entry(range.end + 1).or_default();
    }
    for (index, &(start, end)) in spans.iter().enumerate() {
        if end - start > 1 {
            changes.entry(start + 1).or_default().push((index, true));
            changes.entry(end).or_default().push((index, false));
        }
        for point in [start.saturating_add(769), end.saturating_sub(768)] {
            if start < point && point < end {
                changes.entry(point).or_default();
            }
        }
    }
    let changes: Vec<_> = changes.into_iter().collect();
    let mut active = BTreeSet::new();
    let mut result = LegalCuts {
        ranges: Vec::new(),
        count: 0,
        mobile: true,
    };
    for pair in changes.windows(2) {
        let (start, updates) = &pair[0];
        for &(index, added) in updates {
            if added {
                active.insert(index);
            } else {
                active.remove(&index);
            }
        }
        let end = pair[1].0 - 1;
        if !base.contains(*start) {
            continue;
        }
        if active.is_empty() {
            result.push(*start, end, 1, vec![0]);
            continue;
        }
        let length = end - start + 1;
        // 768틱을 넘는 조각은 MabiIcco에서 576틱씩 줄이므로 같은 나머지는 같은 결과입니다.
        let period = if length > 768 { 576 } else { length };
        let offsets = (0..period)
            .filter(|offset| {
                let tick = start + offset;
                active.iter().all(|&index| {
                    let (a, b) = spans[index];
                    crate::mobile::compatible_duration(tick - a)
                        && crate::mobile::compatible_duration(b - tick)
                })
            })
            .collect();
        result.push(*start, end, period, offsets);
    }
    result
}

/// 구간 길이에 따라 줄어들지 않는 최소 글자 수를 계산합니다.
fn width_lower_bound(score: &Score, tempos: &[Tempo], a: Tick, b: Tick) -> usize {
    let tempo_width: usize = local_tempos(tempos, a, b)
        .iter()
        .map(|(_, v)| 1 + v.to_string().len())
        .sum();
    score
        .tracks
        .iter()
        .flat_map(|track| track.parts.iter().enumerate())
        .map(|(pi, notes)| {
            let note_width = notes
                .iter()
                .filter_map(|n| {
                    let duration = n.off.min(b) - n.on.max(a);
                    (duration > 0).then(|| {
                        // 점온음표는 최대 576틱이며 추가 음가마다 타이와 음이름이 최소 두 글자 필요합니다.
                        let atoms = 1 + (duration - 1) / 576;
                        usize::try_from(atoms)
                            .unwrap_or(usize::MAX / 2)
                            .saturating_mul(2)
                            .saturating_sub(1)
                    })
                })
                .fold(0usize, usize::saturating_add);
            note_width.saturating_add(if pi == 0 { tempo_width } else { 0 })
        })
        .max()
        .unwrap_or(0)
}

/// 이어지는 분할 가능성을 고려해 한도에 맞는 가장 먼 지점을 찾습니다.
fn furthest_fitting(
    score: &Score,
    tempos: &[Tempo],
    legal: &LegalCuts,
    a: Tick,
    limit: usize,
    dead_ends: &BTreeSet<Tick>,
    exact: bool,
) -> Result<Tick> {
    // 양쪽 경계가 각각 유효해도 그 사이 새 음표 조각에는 최소 6틱이 필요합니다.
    let first = legal.first_after(a + MIN_TICK - 1);
    ensure!(first < legal.count, "no valid cut remains after tick {a}");
    let mut lo = first;
    let mut hi = legal.count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if width_lower_bound(score, tempos, a, legal.tick(mid)) <= limit {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let upper = lo;
    ensure!(
        first < upper,
        "a musical event after tick {a} exceeds the {limit}-character limit; increase the limit"
    );
    let last_tick = legal.tick(upper - 1);
    if !dead_ends.contains(&last_tick) && fits(score, tempos, a, last_tick, limit, legal.mobile)? {
        return Ok(last_tick);
    }
    // 기본 음가 압축으로 글자 수가 비단조적이므로 되돌아갈 때는 남은 후보를 모두 확인합니다.
    if exact {
        for rank in (first..upper).rev() {
            let tick = legal.tick(rank);
            if !dead_ends.contains(&tick) && fits(score, tempos, a, tick, limit, legal.mobile)? {
                return Ok(tick);
            }
        }
        bail!(
            "no lossless scroll cut after tick {a} fits {limit} characters per part; increase the limit"
        );
    }
    let mut best = None;
    lo = first;
    hi = upper;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let tick = legal.tick(mid);
        if fits(score, tempos, a, tick, limit, legal.mobile)? {
            if !dead_ends.contains(&tick) {
                best = Some(tick);
            }
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    // 이진 탐색 실패만으로 불가능하다고 결론내리지 않고 제한된 후보를 전부 확인합니다.
    if best.is_none() {
        for rank in first..upper {
            let tick = legal.tick(rank);
            if !dead_ends.contains(&tick) && fits(score, tempos, a, tick, limit, legal.mobile)? {
                best = Some(tick);
                break;
            }
        }
    }
    best.with_context(|| format!("no lossless scroll cut after tick {a} fits {limit} characters per part; increase the limit"))
}

/// 메타데이터에서 박자표 변경을 읽습니다.
fn signatures(tail: &[String]) -> BTreeMap<Tick, String> {
    let mut result = BTreeMap::new();
    let mut in_signatures = false;
    for line in tail {
        let line = line.trim();
        if line.starts_with('[') {
            in_signatures = line == "[time-signature]";
        } else if in_signatures
            && let Some((tick, sig)) = line.split_once('=')
            && let (Ok(tick), Some((num, den))) = (tick.parse::<Tick>(), sig.split_once('/'))
            && tick >= 0
            && num.parse::<u32>().is_ok_and(|n| n > 0)
            && den.parse::<u32>().is_ok_and(|n| n > 0)
        {
            result.insert(tick, sig.to_owned());
        }
    }
    result
}

/// 분할 구간에 맞게 악보의 앞부분 메타데이터를 만듭니다.
fn chunk_head(head: &[String], tempos: &[Tempo], a: Tick, b: Tick) -> Vec<String> {
    let tempo_line = format!(
        "tempo={}",
        local_tempos(tempos, a, b)
            .iter()
            .map(|(t, v)| format!("{t}T{v}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut found = false;
    let mut result: Vec<_> = head
        .iter()
        .map(|line| {
            if line.starts_with("tempo=") {
                found = true;
                tempo_line.clone()
            } else {
                line.clone()
            }
        })
        .collect();
    if !found {
        result.push(tempo_line);
    }
    result
}

/// 분할 구간에 맞게 박자표 등 뒷부분 메타데이터를 만듭니다.
fn chunk_tail(tail: &[String], sigs: &BTreeMap<Tick, String>, a: Tick, b: Tick) -> Vec<String> {
    let current = sigs
        .range(..=a)
        .next_back()
        .map_or("4/4", |(_, sig)| sig.as_str());
    let mut rebased = vec!["[time-signature]".to_owned(), format!("0={current}")];
    rebased.extend(
        sigs.range((std::ops::Bound::Excluded(a), std::ops::Bound::Excluded(b)))
            .map(|(tick, sig)| format!("{}={sig}", tick - a)),
    );
    let mut result = Vec::new();
    let mut skipping = false;
    let mut inserted = false;
    for line in tail {
        if line.trim() == "[time-signature]" {
            if !inserted {
                result.extend(rebased.clone());
                inserted = true;
            }
            skipping = true;
        } else {
            if line.trim().starts_with('[') {
                skipping = false;
            }
            if !skipping {
                result.push(line.clone());
            }
        }
    }
    if !inserted {
        result.extend(rebased);
    }
    result
}

/// 음표 조각과 공통 시간축을 보존하며 악보를 분할합니다.
pub fn split_score(score: &Score, options: &SplitOptions) -> Result<SplitResult> {
    split_score_with_mode(score, options, false)
}

/// 전체 악보에서 보정된 음표 시점을 바꾸지 않고 모바일 호환 구간으로 나눕니다.
pub(crate) fn split_score_mobile(score: &Score, options: &SplitOptions) -> Result<SplitResult> {
    split_score_with_mode(score, options, true)
}

/// 출력 대상의 음가 조건에 맞는 공통 분할 지점을 계획합니다.
fn split_score_with_mode(
    score: &Score,
    options: &SplitOptions,
    mobile: bool,
) -> Result<SplitResult> {
    ensure!(options.limit > 0, "character limit must be positive");
    ensure!(
        options.min_gap >= 0 && options.big_gap >= 0,
        "gap lengths cannot be negative"
    );
    let all_notes = score.all_notes();
    let total = score.total_ticks();
    if all_notes.is_empty() {
        return Ok(SplitResult {
            chunks: Vec::new(),
            report: format!(
                "0 scrolls, limit {} chars/part (score contains no notes)",
                options.limit
            ),
        });
    }
    ensure!(total > 0, "score duration must be positive");
    ensure!(total < Tick::MAX - MIN_TICK, "score duration is too large");
    ensure!(
        all_notes.iter().all(|n| n.on >= 0 && n.off > n.on),
        "notes must have nonnegative onsets and positive durations"
    );
    let tempos = canonical_tempos(score);
    ensure!(
        tempos.iter().all(|(tick, _)| *tick >= 0),
        "tempo event ticks cannot be negative"
    );
    // 원본 인코딩 오류를 경계 이동으로 해결 가능한 분할 오류와 구분합니다.
    chunk_text(score, &tempos, 0, total)
        .context("the input cannot be encoded without changing note timing")?;
    ensure!(
        !mobile || mobile_chunk_compatible(score, &tempos, 0, total),
        "the input must be prepared for mobile before splitting"
    );
    let legal = if mobile {
        mobile_legal_cuts(score, &tempos, total)
    } else {
        legal_cuts(score, &tempos, total)
    };
    let candidates = gaps(&all_notes, options.min_gap);
    let gap_map: BTreeMap<_, _> = gaps(&all_notes, 1).into_iter().collect();
    let sigs = signatures(&score.tail);
    let mut chunks: Vec<ScrollChunk> = Vec::new();
    let mut pos = 0;
    let mut dead_ends = BTreeSet::new();
    let mut exact = false;
    while pos < total {
        let best = match furthest_fitting(
            score,
            &tempos,
            &legal,
            pos,
            options.limit,
            &dead_ends,
            exact,
        ) {
            Ok(tick) => tick,
            Err(error) => {
                dead_ends.insert(pos);
                if let Some(previous) = chunks.pop() {
                    pos = previous.start;
                    exact = true;
                    continue;
                }
                return Err(error);
            }
        };
        let mut end = best;
        if best < total {
            let span = best - pos;
            let window: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|(g, _)| {
                    pos + MIN_TICK < *g
                        && *g <= best
                        && legal.contains(*g)
                        && !dead_ends.contains(g)
                })
                .collect();
            let mut near: Vec<_> = window
                .iter()
                .copied()
                .filter(|(g, _)| (*g - pos) as f64 >= span as f64 * 0.85)
                .collect();
            near.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
            let mut far: Vec<_> = window
                .into_iter()
                .filter(|(g, len)| {
                    (*g - pos) as f64 >= span as f64 * 0.40 && *len >= options.big_gap
                })
                .collect();
            far.sort_unstable_by_key(|(g, _)| std::cmp::Reverse(*g));
            let mut tried = BTreeSet::new();
            for (gap, _) in near.into_iter().chain(far) {
                if tried.insert(gap) && fits(score, &tempos, pos, gap, options.limit, mobile)? {
                    end = gap;
                    break;
                }
            }
        }
        ensure!(end > pos, "split planner did not advance after tick {pos}");
        let texts = chunk_text(score, &tempos, pos, end)?;
        let widths = text_widths(&texts);
        if widths.iter().any(|w| *w > options.limit) {
            bail!(
                "scroll at ticks {pos}..{end} exceeds the {}-character limit",
                options.limit
            );
        }
        let mut tracks = Vec::new();
        for (ti, (original, parts)) in score.tracks.iter().zip(&texts).enumerate() {
            let mut meta = original.meta.clone();
            if mobile {
                meta.insert("name".into(), format!("Track{}", ti + 1));
                for key in ["program", "songProgram", "panpot", "visible"] {
                    meta.entry(key.into()).or_default();
                }
            }
            tracks.push(track_from_mml(
                format!("MML@{};", parts.join(",")),
                meta,
                ti,
            )?);
        }
        chunks.push(ScrollChunk {
            score: Score {
                head: chunk_head(&score.head, &tempos, pos, end),
                tracks,
                tail: chunk_tail(&score.tail, &sigs, pos, end),
            },
            game_parts: texts.clone(),
            mml_parts: texts,
            tempo_note_splits: 0,
            start: pos,
            end,
            widths,
            gap: gap_map.get(&end).copied().unwrap_or(0),
        });
        pos = end;
        exact = false;
    }
    let mut report = format!(
        "{} scrolls, limit {} chars/part\n",
        chunks.len(),
        options.limit
    );
    for (i, chunk) in chunks.iter().enumerate() {
        let gap = if chunk.end == total {
            "end of score".to_owned()
        } else if chunk.gap > 0 {
            format!("{} ticks", chunk.gap)
        } else {
            "none (hard cut)".to_owned()
        };
        let _ = writeln!(
            report,
            "  {:02}  ticks {:7}..{:<7}  widest part {:4}  cut gap {}",
            i + 1,
            chunk.start,
            chunk.end,
            chunk.widths.iter().max().unwrap_or(&0),
            gap
        );
    }
    Ok(SplitResult { chunks, report })
}

/// Python 호환 규칙으로 짧은 조각을 제외하고 구간을 인코딩합니다.
fn python_chunk_text(
    score: &Score,
    tempos: &[Tempo],
    a: Tick,
    b: Tick,
) -> Result<Vec<Vec<String>>> {
    encode_chunk_parts(score, tempos, a, b, true)
}

/// 이진 탐색·마지막 추가 틱·최소 음가 미만 생략을 포함한 Python 분할을 재현합니다.
pub(crate) fn split_score_python(score: &Score, options: &SplitOptions) -> Result<SplitResult> {
    ensure!(options.limit > 0, "character limit must be positive");
    ensure!(
        options.min_gap >= 0 && options.big_gap >= 0,
        "gap lengths cannot be negative"
    );
    let notes = score.all_notes();
    if notes.is_empty() {
        return Ok(SplitResult {
            chunks: Vec::new(),
            report: format!(
                "0 scrolls, limit {} chars/part (score contains no notes)",
                options.limit
            ),
        });
    }
    ensure!(
        notes.iter().all(|n| n.on >= 0 && n.off > n.on),
        "notes must have nonnegative onsets and positive durations"
    );
    let total = score
        .total_ticks()
        .checked_add(1)
        .context("score duration is too large")?;
    ensure!(total < Tick::MAX - MIN_TICK, "score duration is too large");
    let tempos = score
        .tracks
        .iter()
        .find(|track| !track.tempos.is_empty())
        .map_or(&[][..], |track| track.tempos.as_slice());
    let candidates = gaps(&notes, options.min_gap);
    let gap_map: BTreeMap<_, _> = gaps(&notes, 1).into_iter().collect();
    let sigs = signatures(&score.tail);
    let mut chunks = Vec::new();
    let mut pos = 0;
    while pos < total {
        let (mut lo, mut hi, mut best) = (pos + 1, total, None);
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            if text_widths(&python_chunk_text(score, tempos, pos, mid)?)
                .iter()
                .all(|width| *width <= options.limit)
            {
                best = Some(mid);
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        let best = best.context("a single event exceeds the character limit")?;
        let mut end = best;
        if best < total {
            let window: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|(gap, _)| pos + MIN_TICK < *gap && *gap <= best)
                .collect();
            let span = (best - pos) as f64;
            let near = window
                .iter()
                .copied()
                .filter(|(gap, _)| *gap as f64 >= pos as f64 + span * 0.85)
                .max_by_key(|(gap, length)| (*length, *gap));
            let far = window
                .iter()
                .copied()
                .filter(|(gap, length)| {
                    *gap as f64 >= pos as f64 + span * 0.40 && *length >= options.big_gap
                })
                .max_by_key(|(gap, _)| *gap);
            if let Some((gap, _)) = near.or(far) {
                end = gap;
            }
        }
        ensure!(end > pos, "split planner did not advance after tick {pos}");
        let texts = python_chunk_text(score, tempos, pos, end)?;
        let widths = text_widths(&texts);
        ensure!(
            widths.iter().all(|width| *width <= options.limit),
            "scroll at ticks {pos}..{end} exceeds the {}-character limit",
            options.limit
        );
        let tracks = score
            .tracks
            .iter()
            .zip(&texts)
            .enumerate()
            .map(|(ti, (original, parts))| {
                let mut meta = BTreeMap::from([("name".to_owned(), format!("Track{}", ti + 1))]);
                for key in ["program", "songProgram", "panpot", "visible"] {
                    meta.insert(
                        key.to_owned(),
                        original.meta.get(key).cloned().unwrap_or_default(),
                    );
                }
                track_from_mml(format!("MML@{};", parts.join(",")), meta, ti)
            })
            .collect::<Result<_>>()?;
        let mut tail = chunk_tail(&[], &sigs, pos, end);
        tail.push(String::new());
        chunks.push(ScrollChunk {
            score: Score {
                head: chunk_head(&score.head, tempos, pos, end),
                tracks,
                tail,
            },
            game_parts: texts.clone(),
            mml_parts: texts,
            tempo_note_splits: 0,
            start: pos,
            end,
            widths,
            gap: gap_map.get(&end).copied().unwrap_or(0),
        });
        pos = end;
    }
    let mut report = format!(
        "{} scrolls, limit {} chars/part\n",
        chunks.len(),
        options.limit
    );
    for (i, chunk) in chunks.iter().enumerate() {
        let gap = if chunk.gap > 0 {
            format!("{} ticks", chunk.gap)
        } else {
            "none (hard cut)".to_owned()
        };
        let _ = writeln!(
            report,
            "  {:02}  ticks {:7}..{:<7}  widest part {:4}  cut gap {}",
            i + 1,
            chunk.start,
            chunk.end,
            chunk.widths.iter().max().unwrap_or(&0),
            gap
        );
    }
    Ok(SplitResult { chunks, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;

    /// 파트와 공통 템포로 분할 검사 악보를 만듭니다.
    fn score(parts: Vec<Vec<Note>>, tempos: Vec<Tempo>) -> Score {
        Score {
            head: vec!["[mml-score]".into()],
            tracks: vec![Track {
                parts,
                tempos,
                ..Track::default()
            }],
            ..Score::default()
        }
    }

    /// 지정한 구간과 성부 번호의 음표를 만듭니다.
    fn note(on: Tick, off: Tick, pitch: i32, part: usize) -> Note {
        Note {
            on,
            off,
            pitch,
            vel: 12,
            src: (0, part),
        }
    }

    #[test]
    /// 주기형 절단점이 직접 검사와 일치하고 32틱 셋잇단음 경계를 허용하는지 확인합니다.
    fn mobile_cut_ranges_match_direct_fragment_checks() {
        let source = score(
            vec![vec![note(0, 384, 72, 0)], vec![note(8, 344, 48, 1)]],
            vec![(0, 120), (192, 90)],
        );
        let tempos = source.tempos();
        let base = legal_cuts(&source, &tempos, 384);
        let legal = mobile_legal_cuts(&source, &tempos, 384);
        let spans = mobile_spans(&source, &tempos, 384);
        let expected: Vec<_> = (1..=384)
            .filter(|&tick| {
                base.contains(tick)
                    && spans.iter().all(|&(a, b)| {
                        tick <= a
                            || tick >= b
                            || (crate::mobile::compatible_duration(tick - a)
                                && crate::mobile::compatible_duration(b - tick))
                    })
            })
            .collect();
        assert_eq!(legal.count as usize, expected.len());
        for (rank, &tick) in expected.iter().enumerate() {
            assert_eq!(legal.tick(rank as Tick), tick);
            assert_eq!(legal.first_after(tick), rank as Tick + 1);
        }
        for tick in 0..=384 {
            assert_eq!(legal.contains(tick), expected.contains(&tick));
            assert_eq!(
                legal.first_after(tick) as usize,
                expected.partition_point(|t| *t <= tick)
            );
        }
        assert!(legal.contains(32));
        assert!(!legal.contains(19));
    }

    #[test]
    /// 개별 절단점이 유효해도 두 절단점 사이에 생기는 19틱 조각은 거부합니다.
    fn mobile_chunk_checks_the_interval_between_two_cuts() {
        let source = score(vec![vec![note(0, 384, 60, 0)]], vec![(0, 120)]);
        let tempos = source.tempos();
        let legal = mobile_legal_cuts(&source, &tempos, 384);
        assert!(legal.contains(8));
        assert!(legal.contains(27));
        assert!(!fits(&source, &tempos, 8, 27, 100, true).unwrap());
    }

    #[test]
    /// 긴 구간의 모든 틱을 저장하지 않고 576틱 패턴으로 정확히 조회하는지 확인합니다.
    fn mobile_cut_ranges_stay_compact_for_billion_tick_holds() {
        let total = 576 * 10_000_000 + 384;
        let source = score(vec![vec![note(0, total, 60, 0)]], vec![(0, 120)]);
        let legal = mobile_legal_cuts(&source, &source.tempos(), total);
        assert!(legal.ranges.len() < 12);
        assert!(legal.ranges.iter().map(|r| r.offsets.len()).sum::<usize>() < 2400);
        assert!(legal.count > 1_000_000);
        for rank in [0, 1, legal.count / 2, legal.count - 2, legal.count - 1] {
            let tick = legal.tick(rank);
            assert!(legal.contains(tick));
            assert!(crate::mobile::compatible_duration(tick));
            assert!(crate::mobile::compatible_duration(total - tick));
            assert_eq!(legal.first_after(tick), rank + 1);
        }
    }

    #[test]
    /// 여러 반복 주기를 넘는 지속음도 장마다 길이를 바꾸지 않고 끝까지 복원합니다.
    fn mobile_long_hold_splits_without_losing_or_accumulating_ticks() {
        let total = 576 * 32 + 384;
        let source = score(vec![vec![note(0, total, 60, 0)]], vec![(0, 120)]);
        let result = split_score_mobile(
            &source,
            &SplitOptions {
                limit: 12,
                ..SplitOptions::default()
            },
        )
        .unwrap();
        assert!(result.chunks.len() > 1);
        let mut end = 0;
        for chunk in &result.chunks {
            assert_eq!(chunk.start, end);
            assert!(crate::mobile::score_is_compatible(&chunk.score));
            assert_eq!(
                chunk.score.all_notes(),
                vec![note(0, chunk.end - chunk.start, 60, 0)]
            );
            assert!(chunk.widths.iter().all(|width| *width <= 12));
            end = chunk.end;
        }
        assert_eq!(end, total);
    }

    #[test]
    /// 모든 장의 음표·템포를 같은 절대 시점에 보존하고 실제 글자 수와 모바일 음가를 검사합니다.
    fn mobile_split_preserves_triplets_bass_and_tempo_without_per_chunk_retiming() {
        let upper = (0..48)
            .map(|i| note(i * 32, (i + 1) * 32, 60 + (i % 7) as i32, 0))
            .collect();
        let source = score(
            vec![
                upper,
                vec![note(0, 1536, 36, 1)],
                vec![
                    note(64, 128, 48, 2),
                    note(256, 384, 55, 2),
                    note(640, 768, 52, 2),
                ],
            ],
            vec![(0, 120), (320, 90), (960, 150)],
        );
        assert!(crate::mobile::score_is_compatible(&source));
        let result = split_score_mobile(
            &source,
            &SplitOptions {
                limit: 32,
                ..SplitOptions::default()
            },
        )
        .unwrap();
        assert!(result.chunks.len() > 1);
        let mut previous = 0;
        for chunk in &result.chunks {
            assert_eq!(chunk.start, previous);
            assert!(chunk.end > chunk.start);
            assert!(crate::mobile::score_is_compatible(&chunk.score));
            assert!(chunk.widths.iter().all(|width| *width <= 32));
            assert!(
                chunk
                    .mml_parts
                    .iter()
                    .flatten()
                    .all(|text| text.chars().count() <= 32)
            );
            assert_eq!(
                chunk.score.tempos(),
                local_tempos(&source.tempos(), chunk.start, chunk.end)
            );
            for (source_part, part) in source.tracks[0]
                .parts
                .iter()
                .zip(&chunk.score.tracks[0].parts)
            {
                assert_eq!(*part, slice_notes(source_part, chunk.start, chunk.end));
            }
            previous = chunk.end;
        }
        assert_eq!(previous, source.total_ticks());
    }
}
