//! 악보 공통 자료형과 편곡·분할 모듈을 제공합니다.

mod attack_selection;
pub mod core;
mod core_selection;
pub mod fold;
pub mod game_mml;
pub mod input;
pub mod instruments;
pub mod midi;
mod parallel;
pub mod playback;
mod salience;
pub mod split;
pub mod synth;
pub mod verify;
pub mod workflow;

#[cfg(test)]
mod performance_tests;

use std::collections::BTreeMap;

pub type Tick = i64;
pub type Tempo = (Tick, i32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub on: Tick,
    pub off: Tick,
    pub pitch: i32,
    pub vel: i32,
    pub src: (usize, usize),
}

impl Note {
    /// 음표의 틱 길이를 반환합니다.
    pub fn duration(&self) -> Tick {
        self.off - self.on
    }
}

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub mml: String,
    pub meta: BTreeMap<String, String>,
    pub parts: Vec<Vec<Note>>,
    pub tempos: Vec<Tempo>,
}

#[derive(Clone, Debug, Default)]
pub struct Score {
    pub head: Vec<String>,
    pub tracks: Vec<Track>,
    pub tail: Vec<String>,
}

impl Score {
    /// 트랙과 파트 순서로 전체 음표를 복제해 모읍니다.
    pub fn all_notes(&self) -> Vec<Note> {
        self.tracks
            .iter()
            .flat_map(|t| t.parts.iter())
            .flatten()
            .cloned()
            .collect()
    }
    /// 모든 트랙의 템포와 악보 메타데이터를 병합합니다.
    pub fn tempos(&self) -> Vec<Tempo> {
        let mut tempos = BTreeMap::new();
        for track in &self.tracks {
            tempos.extend(track.tempos.iter().copied());
        }
        // 같은 시점의 템포가 겹치면 악보 전체 메타데이터를 우선합니다.
        for line in &self.head {
            if let Some(events) = line.strip_prefix("tempo=") {
                for event in events.split(',') {
                    if let Some((tick, bpm)) = event.split_once('T')
                        && let (Ok(tick), Ok(bpm)) = (tick.parse::<Tick>(), bpm.parse::<i32>())
                        && tick >= 0
                        && bpm > 0
                    {
                        tempos.insert(tick, bpm);
                    }
                }
            }
        }
        tempos.into_iter().collect()
    }
    /// 마지막 음표가 끝나는 틱을 구합니다.
    pub fn total_ticks(&self) -> Tick {
        self.tracks
            .iter()
            .flat_map(|t| t.parts.iter())
            .flatten()
            .map(|n| n.off)
            .max()
            .unwrap_or(0)
    }
}
