//! 변환 전후 악보의 음표·동시발음·MML 표현을 비교합니다.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::{Note, Score, Tick};

/// 동시발음 수별 누적 지속 시간을 계산합니다.
fn polyphony_histogram(notes: &[Note]) -> BTreeMap<usize, Tick> {
    let mut events: BTreeMap<Tick, i64> = BTreeMap::new();
    for note in notes.iter().filter(|n| n.off > n.on) {
        *events.entry(note.on).or_default() += 1;
        *events.entry(note.off).or_default() -= 1;
    }
    let mut histogram = BTreeMap::new();
    let mut current = 0i64;
    let mut previous = 0;
    for (tick, change) in events {
        if tick > previous {
            *histogram.entry(current.max(0) as usize).or_default() += tick - previous;
        }
        current += change;
        previous = tick;
    }
    histogram
}

/// 최대 동시발음 수를 구합니다.
pub fn max_polyphony(notes: &[Note]) -> usize {
    polyphony_histogram(notes)
        .keys()
        .next_back()
        .copied()
        .unwrap_or(0)
}

type NoteKey = (Tick, Tick, i32, i32);

/// 시점·음높이·음량이 같은 음표의 개수를 셉니다.
fn bag(notes: &[Note]) -> BTreeMap<NoteKey, usize> {
    let mut result = BTreeMap::new();
    for n in notes {
        *result.entry((n.on, n.off, n.pitch, n.vel)).or_default() += 1;
    }
    result
}

/// 숫자 값을 제외한 MML 명령 형태별 사용 횟수를 셉니다.
fn shapes(score: &Score) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    for track in &score.tracks {
        let body = track.mml.strip_prefix("MML@").unwrap_or(&track.mml);
        let chars: Vec<_> = body.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            let start = index;
            let ch = chars[index];
            index += 1;
            let lower = ch.to_ascii_lowercase();
            if matches!(ch, '&' | '<' | '>') {
                *result.entry(ch.to_string()).or_default() += 1;
                continue;
            }
            let is_note = matches!(lower, 'a'..='g');
            if !is_note && !matches!(lower, 'r' | 'l' | 'n' | 'o' | 'v' | 't') {
                continue;
            }
            if is_note {
                while index < chars.len() && matches!(chars[index], '+' | '#' | '-') {
                    index += 1;
                }
            }
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            if is_note || matches!(lower, 'r' | 'l') {
                while index < chars.len() && chars[index] == '.' {
                    index += 1;
                }
            }
            let mut shape = String::new();
            let mut in_number = false;
            for &c in &chars[start..index] {
                if c.is_ascii_digit() {
                    if !in_number {
                        shape.push('#');
                    }
                    in_number = true;
                } else {
                    in_number = false;
                    shape.push(c);
                }
            }
            *result.entry(shape).or_default() += 1;
        }
    }
    result
}

/// 음표 보존·동시발음·템포·명령 형태를 비교한 보고서를 만듭니다.
pub fn verify_scores(source: &Score, output: &Score) -> String {
    let source_notes = source.all_notes();
    let output_notes = output.all_notes();
    let source_bag = bag(&source_notes);
    let output_bag = bag(&output_notes);
    let common: usize = output_bag
        .iter()
        .map(|(key, count)| (*count).min(*source_bag.get(key).unwrap_or(&0)))
        .sum();
    let extra: Vec<_> = output_bag
        .iter()
        .filter_map(|(key, count)| {
            let difference = count.saturating_sub(*source_bag.get(key).unwrap_or(&0));
            (difference > 0).then_some((key, difference))
        })
        .collect();
    let histogram = polyphony_histogram(&output_notes);
    let mut report = String::new();
    let _ = writeln!(
        report,
        "out tracks: {}  notes: {}",
        output.tracks.len(),
        output_notes.len()
    );
    let _ = writeln!(report, "out polyphony (voices: ticks): {histogram:?}");
    let _ = writeln!(
        report,
        "max polyphony: {}",
        histogram.keys().next_back().unwrap_or(&0)
    );
    let truth = source.tempos();
    for (i, track) in output.tracks.iter().enumerate() {
        let _ = writeln!(
            report,
            "track{} tempo events exact: {} ({})",
            i + 1,
            track.tempos == truth,
            track.tempos.len()
        );
    }
    let _ = writeln!(
        report,
        "input notes {}, output notes {}, exactly-preserved {} ({:.2}% of output)",
        source_notes.len(),
        output_notes.len(),
        common,
        100.0 * common as f64 / output_notes.len().max(1) as f64
    );
    if !extra.is_empty() {
        let _ = writeln!(
            report,
            "output notes not present verbatim in input: {}",
            extra.iter().map(|(_, count)| count).sum::<usize>()
        );
        for (key, count) in extra.iter().take(8) {
            let _ = writeln!(report, "    {key:?} x{count}");
        }
    }
    let input_shapes = shapes(source);
    let novel: BTreeMap<_, _> = shapes(output)
        .into_iter()
        .filter(|(key, _)| !input_shapes.contains_key(key))
        .collect();
    if novel.is_empty() {
        let _ = writeln!(report, "constructs absent from the source dialect: none");
    } else {
        let _ = writeln!(
            report,
            "constructs absent from the source dialect: {novel:?}"
        );
    }
    report
}
