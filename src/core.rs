//! MMI 컨테이너와 MML을 읽고 음표 시점을 보존해 인코딩합니다.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail, ensure};

use crate::{Note, Score, Tempo, Tick, Track};

pub(crate) const WHOLE: Tick = 384;
pub const MIN_TICK: Tick = 6;
const MAX_DP: usize = 1400;

/// MML 길이와 점 개수를 틱 길이로 계산합니다.
fn duration(length: Tick, dots: usize) -> Result<Tick> {
    ensure!(length >= 0, "Negative MML length {length}");
    let base = if length == 0 { WHOLE } else { WHOLE / length };
    ensure!(base > 0, "MML length {length} has zero duration");
    if dots == 0 {
        return Ok(base);
    }
    // 점 길이는 floor(base × (2 − 2^-dots))를 정수로 계산합니다.
    let remainder = if dots >= 10 {
        1
    } else {
        let denominator = 1_i64 << dots;
        (base + denominator - 1) / denominator
    };
    Ok(base * 2 - remainder)
}

/// 현재 위치의 정수를 읽고 파서 위치를 이동합니다.
fn number(bytes: &[u8], at: &mut usize) -> Result<Option<Tick>> {
    let start = *at;
    let mut value: Tick = 0;
    while *at < bytes.len() && bytes[*at].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(Tick::from(bytes[*at] - b'0')))
            .with_context(|| format!("Number is too large at byte {}", start + 1))?;
        *at += 1;
    }
    Ok((*at > start).then_some(value))
}

/// 명령 뒤의 필수 정수를 읽습니다.
fn required_number(bytes: &[u8], at: &mut usize, command: char) -> Result<Tick> {
    number(bytes, at)?
        .with_context(|| format!("MML command '{command}' requires a number at byte {}", *at))
}

/// 연속된 점을 세고 파서 위치를 이동합니다.
fn dots(bytes: &[u8], at: &mut usize) -> usize {
    let start = *at;
    while *at < bytes.len() && bytes[*at] == b'.' {
        *at += 1;
    }
    *at - start
}

/// 단선율 MML을 음표·템포·전체 길이로 해석합니다.
pub fn parse_part(s: &str, src: (usize, usize)) -> Result<(Vec<Note>, Vec<Tempo>, Tick)> {
    let bytes = s.as_bytes();
    let mut at = 0;
    let mut octave: Tick = 4;
    let mut default_length = 4;
    let mut default_dots = 0;
    let mut volume = 8;
    let mut time: Tick = 0;
    let mut notes: Vec<Note> = Vec::new();
    let mut tempos = Vec::new();
    let mut previous: Option<usize> = None;
    let mut tie = false;

    while at < bytes.len() {
        if bytes[at] == b';' {
            break;
        }
        if bytes[at..]
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"MML@"))
        {
            at += 4;
            continue;
        }
        let command = bytes[at].to_ascii_lowercase();
        at += 1;
        match command {
            b't' | b'v' | b'o' | b'l' => {
                let value = required_number(bytes, &mut at, char::from(command))?;
                match command {
                    b't' => {
                        ensure!(value > 0, "Tempo must be positive at tick {time}");
                        tempos.push((time, i32::try_from(value).context("Tempo is too large")?));
                    }
                    b'v' => volume = i32::try_from(value).context("Volume is too large")?,
                    b'o' => octave = value,
                    b'l' => {
                        default_length = value;
                        default_dots = dots(bytes, &mut at);
                        duration(default_length, default_dots)?;
                    }
                    _ => unreachable!(),
                }
            }
            b'>' => octave = octave.checked_add(1).context("Octave overflow")?,
            b'<' => octave = octave.checked_sub(1).context("Octave overflow")?,
            b'&' => tie = true,
            b'n' | b'r' | b'c' | b'd' | b'e' | b'f' | b'g' | b'a' | b'b' => {
                let (pitch, length) = if command == b'n' {
                    let value = required_number(bytes, &mut at, 'n')?;
                    // n0는 MIDI 0이 아니라 o0c에 해당하는 MIDI 12입니다.
                    let pitch = value.checked_add(12).context("Absolute pitch overflow")?;
                    (Some(pitch), duration(default_length, default_dots)?)
                } else {
                    let mut accidental: Tick = 0;
                    if command != b'r' {
                        while at < bytes.len() && matches!(bytes[at], b'+' | b'#' | b'-') {
                            accidental += if bytes[at] == b'-' { -1 } else { 1 };
                            at += 1;
                        }
                    }
                    let explicit_length = number(bytes, &mut at)?;
                    let explicit_dots = dots(bytes, &mut at);
                    let length = duration(
                        explicit_length.unwrap_or(default_length),
                        if explicit_length.is_some() || explicit_dots > 0 {
                            explicit_dots
                        } else {
                            default_dots
                        },
                    )?;
                    let step = match command {
                        b'c' => 0,
                        b'd' => 2,
                        b'e' => 4,
                        b'f' => 5,
                        b'g' => 7,
                        b'a' => 9,
                        b'b' => 11,
                        _ => 0,
                    };
                    let pitch = if command == b'r' {
                        None
                    } else {
                        Some(
                            octave
                                .checked_add(1)
                                .and_then(|v| v.checked_mul(12))
                                .and_then(|v| v.checked_add(step))
                                .and_then(|v| v.checked_add(accidental))
                                .context("Pitch overflow")?,
                        )
                    };
                    (pitch, length)
                };
                let end = time
                    .checked_add(length)
                    .context("Score duration overflow")?;
                if let Some(pitch) = pitch {
                    let pitch = i32::try_from(pitch).context("Pitch is too large")?;
                    if let Some(previous) = previous.filter(|&i| tie && notes[i].pitch == pitch) {
                        notes[previous].off = end;
                    } else {
                        notes.push(Note {
                            on: time,
                            off: end,
                            pitch,
                            vel: volume,
                            src,
                        });
                        previous = Some(notes.len() - 1);
                    }
                } else {
                    previous = None;
                }
                tie = false;
                time = end;
            }
            // 공백과 내보내기 도구의 부가 문자는 원본 파서처럼 무시합니다.
            _ => {}
        }
    }
    Ok((notes, tempos, time))
}

/// 여러 MML 파트와 메타데이터로 트랙을 만듭니다.
pub fn track_from_mml(
    mml: String,
    meta: BTreeMap<String, String>,
    track_index: usize,
) -> Result<Track> {
    let text = mml.trim();
    let mut normalized = if text
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("MML@"))
    {
        text.to_owned()
    } else {
        format!("MML@{text}")
    };
    if !normalized.ends_with(';') {
        normalized.push(';');
    }
    let mut parts = Vec::new();
    let mut tempos = BTreeMap::new();
    for (part_index, part) in normalized.split(',').enumerate() {
        let (notes, part_tempos, _) = parse_part(part, (track_index, part_index))
            .with_context(|| format!("Track {}, part {}", track_index + 1, part_index + 1))?;
        parts.push(notes);
        // 템포가 여러 파트에 분산될 수 있으므로 모든 시점을 모으고 중복만 제거합니다.
        tempos.extend(part_tempos);
    }
    Ok(Track {
        mml: normalized,
        meta,
        parts,
        tempos: tempos.into_iter().collect(),
    })
}

/// MMI의 트랙과 앞뒤 메타데이터를 읽습니다.
pub fn parse_mmi(raw: &str) -> Result<Score> {
    let mut score = Score::default();
    let mut current: Option<(String, BTreeMap<String, String>)> = None;
    let mut seen_track = false;
    let mut in_tail = false;
    for raw_line in raw.trim_start_matches('\u{feff}').split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if let Some(mml) = line.strip_prefix("mml-track=") {
            if let Some((mml, meta)) = current.take() {
                score
                    .tracks
                    .push(track_from_mml(mml, meta, score.tracks.len())?);
            }
            ensure!(!in_tail, "Track appears after a trailing score section");
            current = Some((mml.to_owned(), BTreeMap::new()));
            seen_track = true;
        } else if seen_track && line.starts_with('[') {
            if let Some((mml, meta)) = current.take() {
                score
                    .tracks
                    .push(track_from_mml(mml, meta, score.tracks.len())?);
            }
            in_tail = true;
            score.tail.push(line.to_owned());
        } else if let Some((_, meta)) = current.as_mut() {
            if let Some((key, value)) = line.split_once('=') {
                meta.insert(key.to_owned(), value.to_owned());
                if key == "visible" {
                    let (mml, meta) = current.take().expect("current track exists");
                    score
                        .tracks
                        .push(track_from_mml(mml, meta, score.tracks.len())?);
                }
            } else if !line.is_empty() {
                bail!("Invalid track metadata line: {line}");
            }
        } else if !seen_track {
            score.head.push(line.to_owned());
        } else {
            score.tail.push(line.to_owned());
        }
    }
    if let Some((mml, meta)) = current {
        score
            .tracks
            .push(track_from_mml(mml, meta, score.tracks.len())?);
    }
    Ok(score)
}

/// 파일의 MMI 악보를 읽습니다.
pub fn load_mmi(path: &Path) -> Result<Score> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read {} as UTF-8", path.display()))?;
    parse_mmi(&raw).with_context(|| format!("Cannot parse {}", path.display()))
}

/// 악보와 메타데이터를 MMI 문자열로 저장합니다.
pub fn serialize_mmi(score: &Score) -> String {
    let mut lines = score.head.clone();
    for track in &score.tracks {
        lines.push(format!("mml-track={}", track.mml));
        for key in ["name", "program", "songProgram", "panpot"] {
            if let Some(value) = track.meta.get(key) {
                lines.push(format!("{key}={value}"));
            }
        }
        for (key, value) in &track.meta {
            if !matches!(
                key.as_str(),
                "name" | "program" | "songProgram" | "panpot" | "visible"
            ) {
                lines.push(format!("{key}={value}"));
            }
        }
        // Python 로더는 visible 행에서 트랙을 닫으므로 마지막에 씁니다.
        if let Some(value) = track.meta.get("visible") {
            lines.push(format!("visible={value}"));
        }
    }
    lines.extend(score.tail.iter().cloned());
    lines.join("\n")
}

struct LengthTable {
    atoms: Vec<(Tick, String)>,
    decompositions: Vec<Option<Vec<Tick>>>,
    largest: Tick,
}

/// 표현 가능한 MML 음가와 틱 분해 표를 준비합니다.
fn lengths() -> &'static LengthTable {
    static TABLE: OnceLock<LengthTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut atoms: Vec<(Tick, String)> = Vec::new();
        // 점은 하나만 허용하며 동률의 음가 분해는 Python 인코더의 삽입 순서를 따릅니다.
        for length in 1..=64 {
            for dot_count in 0..=1 {
                let ticks = duration(length, dot_count).expect("valid atom");
                let token = format!("{length}{}", ".".repeat(dot_count));
                if let Some((_, old)) = atoms.iter_mut().find(|(d, _)| *d == ticks) {
                    if token.len() < old.len() {
                        *old = token;
                    }
                } else {
                    atoms.push((ticks, token));
                }
            }
        }
        let mut decompositions: Vec<Option<Vec<Tick>>> = vec![None; MAX_DP + 1];
        decompositions[0] = Some(Vec::new());
        for value in 1..=MAX_DP {
            let mut best: Option<Vec<Tick>> = None;
            for (atom, _) in &atoms {
                if *atom <= value as Tick
                    && let Some(previous) = &decompositions[value - *atom as usize]
                    && best.as_ref().is_none_or(|b| previous.len() + 1 < b.len())
                {
                    let mut candidate = previous.clone();
                    candidate.push(*atom);
                    best = Some(candidate);
                }
            }
            decompositions[value] = best;
        }
        let largest = atoms.iter().map(|(d, _)| *d).max().expect("nonempty atoms");
        LengthTable {
            atoms,
            decompositions,
            largest,
        }
    })
}

/// 틱 길이를 반올림 없이 표현 가능한 음가로 분해합니다.
pub fn split_ticks(mut ticks: Tick) -> Result<Vec<Tick>> {
    ensure!(ticks >= 0, "Negative duration {ticks}");
    let original = ticks;
    let table = lengths();
    let mut result = Vec::new();
    while ticks > MAX_DP as Tick {
        result.push(table.largest);
        ticks -= table.largest;
    }
    let rest = table.decompositions[ticks as usize]
        .as_ref()
        .with_context(|| {
            format!("Unrepresentable duration {original} ticks (minimum is {MIN_TICK})")
        })?;
    result.extend(rest);
    Ok(result)
}

/// 음높이를 표현할 수 있는 옥타브와 음이름 후보를 만듭니다.
fn spellings(pitch: i32) -> Vec<(i32, &'static str)> {
    const SHARP: [&str; 12] = [
        "c", "c+", "d", "d+", "e", "f", "f+", "g", "g+", "a", "a+", "b",
    ];
    let octave = pitch.div_euclid(12) - 1;
    let step = pitch.rem_euclid(12);
    let mut options = vec![(octave, SHARP[step as usize])];
    match step {
        0 => options.push((octave - 1, "b+")),
        11 => options.push((octave + 1, "c-")),
        // E#/Fb는 B#/Cb와 달리 옥타브 경계를 넘지 않습니다.
        5 => options.push((octave, "e+")),
        4 => options.push((octave, "f-")),
        _ => {}
    }
    options
}

struct Writer {
    output: String,
    octave: i32,
    volume: i32,
    default_token: String,
    time: Tick,
}

impl Writer {
    /// MML 출력 상태를 초기화합니다.
    fn new() -> Self {
        Self {
            output: String::new(),
            octave: 4,
            volume: 8,
            default_token: "4".into(),
            time: 0,
        }
    }

    /// 현재 기본 음가와 다음 사용 횟수를 고려해 길이 표기를 고릅니다.
    fn length_token(
        &mut self,
        ticks: Tick,
        upcoming: &BTreeMap<Tick, usize>,
        allow_default: bool,
    ) -> String {
        let token = &lengths()
            .atoms
            .iter()
            .find(|(d, _)| *d == ticks)
            .expect("representable atom")
            .1;
        if *token == self.default_token {
            String::new()
        } else if allow_default && upcoming.get(&ticks).copied().unwrap_or(0) >= 3 {
            self.output.push('l');
            self.output.push_str(token);
            self.default_token = token.clone();
            String::new()
        } else {
            token.clone()
        }
    }

    /// 옥타브 이동 명령을 출력합니다.
    fn set_octave(&mut self, target: i32) {
        let difference = target - self.octave;
        if difference.abs() <= 2 {
            self.output.push_str(
                &if difference > 0 { ">" } else { "<" }.repeat(difference.unsigned_abs() as usize),
            );
        } else {
            self.output.push_str(&format!("o{target}"));
        }
        self.octave = target;
    }

    /// 템포 명령을 출력합니다.
    fn tempo(&mut self, value: i32) {
        self.output.push_str(&format!("t{value}"));
    }

    /// 지정한 틱 길이의 쉼표를 출력합니다.
    fn rest(&mut self, ticks: Tick, upcoming: &BTreeMap<Tick, usize>) -> Result<()> {
        for atom in split_ticks(ticks).with_context(|| format!("Rest at tick {}", self.time))? {
            let token = self.length_token(atom, upcoming, true);
            self.output.push('r');
            self.output.push_str(&token);
        }
        self.time = self
            .time
            .checked_add(ticks)
            .context("Score duration overflow")?;
        Ok(())
    }

    /// 음높이·음량·길이를 MML 음표로 출력합니다.
    fn note(
        &mut self,
        note: &Note,
        upcoming: &BTreeMap<Tick, usize>,
        tempo_points: &[Tempo],
    ) -> Result<()> {
        if note.vel != self.volume {
            self.output.push_str(&format!("v{}", note.vel));
            self.volume = note.vel;
        }
        let (octave, name) = spellings(note.pitch)
            .into_iter()
            .filter(|(octave, _)| (0..=8).contains(octave))
            .min_by_key(|(octave, name)| {
                let distance = (octave - self.octave).unsigned_abs() as usize;
                (if distance <= 2 {
                    distance
                } else {
                    format!("o{octave}").len()
                }) + name.len()
            })
            .with_context(|| {
                format!(
                    "Pitch {} at tick {} is outside encodable octaves 0..8",
                    note.pitch, note.on
                )
            })?;
        self.set_octave(octave);
        let mut first = true;
        let mut start = note.on;
        for (end, tempo) in tempo_points
            .iter()
            .map(|&(t, v)| (t, Some(v)))
            .chain(std::iter::once((note.off, None)))
        {
            for atom in split_ticks(end - start)
                .with_context(|| format!("Note {} at tick {start}", note.pitch))?
            {
                // 기본 음가 명령은 타이 앞에 놓아 지속음 중간의 길이 해석을 바꾸지 않습니다.
                let token = self.length_token(atom, upcoming, first);
                if !first {
                    self.output.push('&');
                }
                self.output.push_str(name);
                self.output.push_str(&token);
                first = false;
            }
            if let Some(tempo) = tempo {
                self.tempo(tempo);
            }
            start = end;
        }
        self.time = note.off;
        Ok(())
    }
}

/// 단선율 음표와 템포를 인코딩하며 끝의 쉼표는 생략합니다.
pub fn emit_part(notes: &[Note], tempos: &[Tempo], total: Tick) -> Result<String> {
    ensure!(total >= 0, "Negative total duration {total}");
    let mut ordered_notes: Vec<&Note> = notes.iter().collect();
    ordered_notes.sort_by_key(|n| n.on);
    let mut upcoming = BTreeMap::new();
    let mut previous_end = 0;
    for note in &ordered_notes {
        ensure!(
            note.on >= 0 && note.off > note.on,
            "Invalid note interval {}..{}",
            note.on,
            note.off
        );
        ensure!(
            note.on >= previous_end,
            "Overlapping notes in one part at tick {}",
            note.on
        );
        ensure!(note.vel >= 0, "Negative volume at tick {}", note.on);
        for atom in split_ticks(note.duration())? {
            *upcoming.entry(atom).or_default() += 1;
        }
        previous_end = note.off;
    }
    let mut tempos = tempos.to_vec();
    tempos.sort();
    for &(tick, value) in &tempos {
        ensure!(
            tick >= 0 && value > 0,
            "Invalid tempo {value} at tick {tick}"
        );
    }
    let mut writer = Writer::new();
    let mut tempo_index = 0;
    for note in ordered_notes {
        while tempo_index < tempos.len() && tempos[tempo_index].0 <= note.on {
            let (tick, value) = tempos[tempo_index];
            if tick > writer.time {
                writer.rest(tick - writer.time, &upcoming)?;
            }
            writer.tempo(value);
            tempo_index += 1;
        }
        if note.on > writer.time {
            writer.rest(note.on - writer.time, &upcoming)?;
        }
        let first_inside = tempo_index;
        while tempo_index < tempos.len() && tempos[tempo_index].0 < note.off {
            tempo_index += 1;
        }
        writer.note(note, &upcoming, &tempos[first_inside..tempo_index])?;
    }
    for &(tick, value) in &tempos[tempo_index..] {
        if tick > writer.time {
            writer.rest(tick - writer.time, &upcoming)?;
        }
        writer.tempo(value);
    }
    Ok(writer.output)
}
