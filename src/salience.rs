//! 원본의 과거 음표 특징과 내장 신경망으로 핵심음 확률을 계산합니다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::{Note, Score};

const INPUTS: usize = 24;
const HIDDEN: usize = 16;
const BANK: &[u8] = include_bytes!("../assets/melody/core-v2.bin");

struct Model {
    mean: [f64; INPUTS],
    deviation: [f64; INPUTS],
    weights: [[f64; HIDDEN]; INPUTS],
    bias: [f64; HIDDEN],
    output: [f64; HIDDEN],
    intercept: f64,
}

impl Model {
    /// 내장 가중치를 한 번 읽고 모델 구조를 확인합니다.
    fn embedded() -> &'static Self {
        static MODEL: OnceLock<Model> = OnceLock::new();
        MODEL.get_or_init(|| {
            assert_eq!(&BANK[..8], b"MMLCORE1");
            let header: Vec<_> = BANK[8..24]
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            assert_eq!(header, [1, INPUTS as u32, HIDDEN as u32, 1]);
            let mut values = BANK[24..]
                .chunks_exact(4)
                .map(|b| f64::from(f32::from_le_bytes(b.try_into().unwrap())));
            let mut take = || values.next().expect("Complete embedded model");
            let model = Self {
                mean: std::array::from_fn(|_| take()),
                deviation: std::array::from_fn(|_| take()),
                weights: std::array::from_fn(|_| std::array::from_fn(|_| take())),
                bias: std::array::from_fn(|_| take()),
                output: std::array::from_fn(|_| take()),
                intercept: take(),
            };
            assert!(values.next().is_none());
            assert!(model.deviation.iter().all(|v| v.is_finite() && *v > 0.0));
            model
        })
    }

    /// 정규화한 특징을 신경망에 통과시켜 로짓을 계산합니다.
    fn logit(&self, features: &[f64; INPUTS]) -> f64 {
        let mut hidden = self.bias;
        for (i, &feature) in features.iter().enumerate() {
            let value = (feature - self.mean[i]) / self.deviation[i];
            for (h, output) in hidden.iter_mut().enumerate() {
                *output += value * self.weights[i][h];
            }
        }
        self.intercept
            + hidden
                .iter()
                .zip(self.output)
                .map(|(v, w)| v.tanh() * w)
                .sum::<f64>()
    }
}

struct History {
    notes: Vec<usize>,
    run_start: i64,
}

/// 동음의 평균 순위를 반영한 상대 음높이 순위를 구합니다.
fn rank(pitch: i32, peers: &[usize], notes: &[&Note]) -> f64 {
    if peers.len() <= 1 {
        return 0.5;
    }
    let below = peers.iter().filter(|&&i| notes[i].pitch < pitch).count();
    let equal = peers.iter().filter(|&&i| notes[i].pitch == pitch).count();
    (below as f64 + (equal as f64 - 1.0) * 0.5) / (peers.len() - 1) as f64
}

/// 원본 음표 순서를 유지하며 각 공격의 인과적 특징 24개를 계산합니다.
pub(crate) fn features(score: &Score) -> Vec<[f64; INPUTS]> {
    let mut notes = Vec::new();
    let mut sources = Vec::new();
    for (ti, track) in score.tracks.iter().enumerate() {
        for (pi, part) in track.parts.iter().enumerate() {
            for note in part {
                notes.push(note);
                sources.push((ti, pi));
            }
        }
    }
    let mut rows = vec![[0.0; INPUTS]; notes.len()];
    let mut onsets = BTreeMap::<i64, Vec<usize>>::new();
    for (i, n) in notes.iter().enumerate() {
        onsets.entry(n.on).or_default().push(i);
    }
    let mut histories = BTreeMap::<(usize, usize), History>::new();
    let mut active = Vec::<usize>::new();
    for (onset, peers) in onsets {
        active.retain(|&i| notes[i].off > onset);
        active.extend(&peers);
        let top = |group: &[usize]| group.iter().map(|&i| notes[i].pitch).max().unwrap();
        let mean = |group: &[usize]| {
            group
                .iter()
                .map(|&i| f64::from(notes[i].pitch))
                .sum::<f64>()
                / group.len() as f64
        };
        let onset_top = top(&peers);
        let active_top = top(&active);
        let onset_mean = mean(&peers);
        let active_mean = mean(&active);
        let mean_duration = peers
            .iter()
            .map(|&i| notes[i].duration() as f64)
            .sum::<f64>()
            / peers.len() as f64;
        let max_velocity = peers.iter().map(|&i| notes[i].vel).max().unwrap().max(1);
        let mut pending = Vec::new();
        for &i in &peers {
            let note = notes[i];
            let prior = histories.get(&sources[i]);
            let previous = prior
                .and_then(|h| h.notes.last())
                .copied()
                .filter(|&j| onset - notes[j].off <= 384);
            let history: Vec<_> = prior
                .into_iter()
                .flat_map(|h| h.notes.iter())
                .copied()
                .filter(|&j| notes[j].off > onset - 384)
                .collect();
            let delta = previous.map_or(0, |j| note.pitch - notes[j].pitch);
            let same = previous.is_some() && delta == 0;
            let beginning = if same && previous.is_some_and(|j| onset - notes[j].off <= 12) {
                prior.unwrap().run_start
            } else {
                onset
            };
            let durations: Vec<_> = history
                .iter()
                .map(|&j| (onset.min(notes[j].off) - (onset - 384).max(notes[j].on)).max(0))
                .collect();
            let weight: i64 = durations.iter().sum();
            let history_mean = if weight > 0 {
                history
                    .iter()
                    .zip(&durations)
                    .map(|(&j, &d)| f64::from(notes[j].pitch) * d as f64)
                    .sum::<f64>()
                    / weight as f64
            } else {
                f64::from(note.pitch)
            };
            let mut occupied = 0;
            let mut cursor = onset - 384;
            for &j in &history {
                let left = (onset - 384).max(notes[j].on);
                let right = onset.min(notes[j].off);
                if right > left.max(cursor) {
                    occupied += right - left.max(cursor);
                    cursor = right;
                }
            }
            let beat = if onset % 96 == 0 {
                1.0
            } else if onset % 48 == 0 {
                2.0 / 3.0
            } else if onset % 24 == 0 {
                1.0 / 3.0
            } else {
                0.0
            };
            let history_span = if history.is_empty() {
                0
            } else {
                top(&history) - history.iter().map(|&j| notes[j].pitch).min().unwrap()
            };
            rows[i] = [
                (note.duration() as f64 / 96.0).min(8.0) / 8.0,
                (f64::from(note.vel) / 15.0).clamp(0.0, 1.0),
                rank(note.pitch, &peers, &notes),
                rank(note.pitch, &active, &notes),
                (f64::from(onset_top - note.pitch) / 48.0).min(1.0),
                (f64::from(active_top - note.pitch) / 48.0).min(1.0),
                ((f64::from(note.pitch) - onset_mean) / 48.0).clamp(-1.0, 1.0),
                ((f64::from(note.pitch) - active_mean) / 48.0).clamp(-1.0, 1.0),
                peers.len().min(16) as f64 / 16.0,
                active.len().min(16) as f64 / 16.0,
                previous.map_or(1.0, |j| {
                    ((onset - notes[j].off) as f64 / 384.0).clamp(0.0, 1.0)
                }),
                (f64::from(delta) / 24.0).clamp(-1.0, 1.0),
                (f64::from(delta).abs() / 24.0).min(1.0),
                f64::from(same),
                f64::from(previous.is_some()),
                (f64::from(history_span) / 48.0).min(1.0),
                ((f64::from(note.pitch) - history_mean) / 48.0).clamp(-1.0, 1.0),
                if history.is_empty() {
                    0.0
                } else {
                    history
                        .iter()
                        .map(|&j| notes[j].pitch)
                        .collect::<BTreeSet<_>>()
                        .len() as f64
                        / history.len() as f64
                },
                ((note.off - beginning) as f64 / 768.0).min(1.0),
                prior
                    .map_or(0, |h| {
                        h.notes
                            .iter()
                            .filter(|&&j| notes[j].on >= onset - 96)
                            .count()
                    })
                    .min(8) as f64
                    / 8.0,
                beat,
                (note.duration() as f64 / mean_duration).min(4.0) / 4.0,
                (f64::from(note.vel) / f64::from(max_velocity)).clamp(0.0, 1.0),
                occupied as f64 / 384.0,
            ];
            pending.push((i, beginning));
        }
        for (i, beginning) in pending {
            let history = histories.entry(sources[i]).or_insert_with(|| History {
                notes: Vec::new(),
                run_start: onset,
            });
            history.notes.push(i);
            history
                .notes
                .retain(|&j| notes[j].off > onset - 384 || j == i);
            history.run_start = beginning;
        }
    }
    rows
}

/// 음표별 특징을 핵심음 확률로 변환합니다.
pub(crate) fn probabilities(score: &Score) -> Vec<f64> {
    let model = Model::embedded();
    let rows = features(score);
    crate::parallel::map(&rows, rows.len(), |row| {
        // 각 행의 부동소수점 합산 순서는 병렬화 전과 같습니다.
        let logit = model.logit(row).clamp(-40.0, 40.0);
        1.0 / (1.0 + (-logit).exp())
    })
}

#[cfg(test)]
mod tests {
    use super::{Model, features, probabilities};
    use crate::{Note, Score, Track};
    use serde_json::Value;
    use std::path::Path;

    /// 수치 비교 자료에서 검사 악보를 만듭니다.
    fn score_from_fixture(case: &Value) -> Score {
        let tracks = case["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| {
                let parts = track["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|part| {
                        part.as_array()
                            .unwrap()
                            .iter()
                            .map(|note| Note {
                                on: note["on"].as_i64().unwrap(),
                                off: note["off"].as_i64().unwrap(),
                                pitch: note["pitch"].as_i64().unwrap().try_into().unwrap(),
                                vel: note["vel"].as_i64().unwrap().try_into().unwrap(),
                                src: (
                                    note["src"][0].as_u64().unwrap().try_into().unwrap(),
                                    note["src"][1].as_u64().unwrap().try_into().unwrap(),
                                ),
                            })
                            .collect()
                    })
                    .collect();
                Track {
                    parts,
                    ..Track::default()
                }
            })
            .collect();
        Score {
            tracks,
            ..Score::default()
        }
    }

    /// 수치가 허용 오차 안에서 일치하는지 확인합니다.
    fn close(actual: f64, expected: f64, tolerance: f64, context: &str) {
        assert!(
            actual.is_finite() && (actual - expected).abs() <= tolerance,
            "{context}: actual={actual:.16}, expected={expected:.16}, error={:.3e}",
            (actual - expected).abs()
        );
    }

    /// 한 음표의 특징과 모델 계산값을 기준 자료와 비교합니다.
    fn verify_row(
        actual: &[f64; 24],
        expected: &Value,
        actual_probability: f64,
        expected_logit: f64,
        expected_probability: f64,
        context: &str,
    ) {
        let expected = expected.as_array().unwrap();
        assert_eq!(actual.len(), expected.len());
        for (feature, (&actual, expected)) in actual.iter().zip(expected).enumerate() {
            close(
                actual,
                expected.as_f64().unwrap(),
                1e-10,
                &format!("{context} feature {feature}"),
            );
        }
        close(
            Model::embedded().logit(actual),
            expected_logit,
            1e-9,
            &format!("{context} logit"),
        );
        close(
            actual_probability,
            expected_probability,
            1e-10,
            &format!("{context} probability"),
        );
    }

    #[test]
    /// Python 기준 특징과 내장 모델의 수치 일치를 검사합니다.
    fn python_golden_features_and_embedded_model_match() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/core-model-features.json"))
                .unwrap();
        let metadata: Value =
            serde_json::from_str(include_str!("../assets/melody/core-v2.json")).unwrap();
        assert_eq!(fixture["feature_version"], 1);
        assert_eq!(fixture["model_sha256"], metadata["sha256"]);
        assert_eq!(fixture["feature_names"], metadata["features"]);
        let mut checked_notes = 0;
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let score = score_from_fixture(case);
            let actual_features = features(&score);
            let actual_probabilities = probabilities(&score);
            let expected_features = case["features"].as_array().unwrap();
            assert_eq!(actual_features.len(), expected_features.len(), "{name}");
            assert_eq!(
                actual_probabilities.len(),
                expected_features.len(),
                "{name}"
            );
            for (index, (actual, expected)) in
                actual_features.iter().zip(expected_features).enumerate()
            {
                verify_row(
                    actual,
                    expected,
                    actual_probabilities[index],
                    case["logits"][index].as_f64().unwrap(),
                    case["probabilities"][index].as_f64().unwrap(),
                    &format!("{name} note {index}"),
                );
            }
            checked_notes += actual_features.len();
        }
        assert_eq!(checked_notes, 121);
    }

    #[test]
    #[ignore = "requires the user-provided source and local Python expectation file"]
    /// MML_CORE_PARITY_EXPECTED로 지정한 비공개 코퍼스의 Python 결과와 비교합니다.
    fn private_corpus_notes_match_python() {
        let expected_path = std::env::var("MML_CORE_PARITY_EXPECTED")
            .expect("Set MML_CORE_PARITY_EXPECTED to a complete-source expectation JSON");
        let expected: Value =
            serde_json::from_slice(&std::fs::read(expected_path).unwrap()).unwrap();
        let metadata: Value =
            serde_json::from_str(include_str!("../assets/melody/core-v2.json")).unwrap();
        assert_eq!(expected["model_sha256"], metadata["sha256"]);
        if let Some(sources) = metadata["training"]["sources"].as_array() {
            assert!(
                sources
                    .iter()
                    .any(|s| s["source_sha256"] == expected["source_sha256"])
            );
        } else {
            assert_eq!(
                expected["source_sha256"],
                metadata["training"]["source_sha256"]
            );
        }
        let score =
            crate::core::load_mmi(Path::new(expected["source_path"].as_str().unwrap())).unwrap();
        let rows = expected["rows"].as_array().unwrap();
        let notes = score.all_notes();
        let actual_features = features(&score);
        let actual_probabilities = probabilities(&score);
        assert_eq!(notes.len(), rows.len());
        let mut maximum_feature_error = 0.0_f64;
        let mut maximum_probability_error = 0.0_f64;
        for (index, (note, row)) in notes.iter().zip(rows).enumerate() {
            assert_eq!(row["source_index"].as_u64().unwrap(), index as u64);
            assert_eq!(
                (
                    note.on,
                    note.off,
                    i64::from(note.pitch),
                    i64::from(note.vel)
                ),
                (
                    row["on"].as_i64().unwrap(),
                    row["off"].as_i64().unwrap(),
                    row["pitch"].as_i64().unwrap(),
                    row["vel"].as_i64().unwrap()
                ),
                "Original event {index} differs before feature extraction"
            );
            for (&actual, expected) in actual_features[index]
                .iter()
                .zip(row["features"].as_array().unwrap())
            {
                maximum_feature_error =
                    maximum_feature_error.max((actual - expected.as_f64().unwrap()).abs());
            }
            let expected_probability = row["probability"].as_f64().unwrap();
            maximum_probability_error = maximum_probability_error
                .max((actual_probabilities[index] - expected_probability).abs());
            verify_row(
                &actual_features[index],
                &row["features"],
                actual_probabilities[index],
                row["logit"].as_f64().unwrap(),
                expected_probability,
                &format!("Note {index}"),
            );
        }
        println!(
            "{} original notes matched Python: max feature error={maximum_feature_error:.3e}, \
             max probability error={maximum_probability_error:.3e}",
            notes.len()
        );
    }
}
