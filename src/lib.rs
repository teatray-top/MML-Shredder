//! 악보 공통 자료형과 편곡·분할 모듈을 제공합니다.

mod attack_selection;
pub mod core;
mod core_selection;
pub mod fold;
pub mod game_mml;
mod harmony;
pub mod input;
pub mod instruments;
pub mod midi;
mod mobile;
mod parallel;
pub mod playback;
mod salience;
#[cfg(target_arch = "wasm32")]
extern crate self as mmlfold;
#[cfg(target_arch = "wasm32")]
mod gui;
pub mod split;
pub mod synth;
#[cfg(target_arch = "wasm32")]
mod typography;
pub mod verify;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub mod web_audio;
#[cfg(target_arch = "wasm32")]
mod web_file;
#[cfg(target_arch = "wasm32")]
pub mod web_jobs;
pub mod workflow;

#[cfg(test)]
mod performance_tests;

use std::collections::BTreeMap;

/// 빌드 시 생성한 공유 아이콘을 웹 화면에 제공합니다.
#[cfg(target_arch = "wasm32")]
fn window_icon() -> eframe::egui::IconData {
    let rgba: &[u8; 256 * 256 * 4] = include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.rgba"));
    eframe::egui::IconData {
        rgba: rgba.to_vec(),
        width: 256,
        height: 256,
    }
}

pub type Tick = i64;
pub type Tempo = (Tick, i32);

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
    pub mml: String,
    pub meta: BTreeMap<String, String>,
    pub parts: Vec<Vec<Note>>,
    pub tempos: Vec<Tempo>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(target_arch = "wasm32", derive(serde::Serialize, serde::Deserialize))]
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
