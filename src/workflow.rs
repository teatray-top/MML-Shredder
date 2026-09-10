//! GUI와 CLI의 편곡·분할 작업 및 파일 저장을 연결합니다.
use crate::{
    Score, core,
    fold::{self, FoldOptions},
    split::{self, SplitOptions},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Artifact {
    pub name: String,
    pub text: String,
    pub game_parts: Vec<Vec<String>>,
    pub tempo_note_splits: usize,
}

pub struct GameExport {
    pub parts: Vec<Vec<String>>,
    pub tempo_note_splits: usize,
}

/// 기존 MML 문자열에서 파트별 내보내기 정보를 추출합니다.
pub fn game_export(score: &Score) -> Result<GameExport> {
    // 다시 인코딩하면 파트 사이 템포 위치가 달라질 수 있으므로 기존 문자열을 그대로 씁니다.
    let parts = score
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let body = track
                .mml
                .strip_prefix("MML@")
                .and_then(|text| text.strip_suffix(';'))
                .with_context(|| format!("Track {} has no complete MML payload", index + 1))?;
            Ok(body.split(',').map(str::to_owned).collect())
        })
        .collect::<Result<_>>()?;
    Ok(GameExport {
        parts,
        tempo_note_splits: 0,
    })
}

/// 트랙과 파트의 MML을 쉼표로 연결합니다.
pub fn game_text(parts: &[Vec<String>]) -> String {
    let mut text = String::new();
    for (ti, track) in parts.iter().enumerate() {
        for (pi, part) in track.iter().enumerate() {
            text.push_str(&format!(
                "--- track{} part{} ({} chars) ---\n{}\n",
                ti + 1,
                pi + 1,
                part.chars().count(),
                part
            ));
        }
    }
    text
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    Import,
    Fold,
    Split,
    Verify,
}

#[derive(Clone)]
pub struct SplitChunkPreview {
    pub start: i64,
    pub end: i64,
    pub part_widths: Vec<Vec<usize>>,
}

#[derive(Clone)]
pub struct SplitPreview {
    pub limit: usize,
    pub chunks: Vec<SplitChunkPreview>,
}

#[derive(Clone)]
pub struct WorkResult {
    pub kind: WorkKind,
    pub score: Score,
    pub report: String,
    pub artifacts: Vec<Artifact>,
    pub split: Option<SplitPreview>,
}

/// 음표와 편성을 바꾸지 않고 입력 악보의 MMI 저장 결과를 만듭니다.
pub fn source_export(score: &Score, stem: &str) -> Result<WorkResult> {
    let game = game_export(score)?;
    Ok(WorkResult {
        kind: WorkKind::Import,
        score: score.clone(),
        report: String::new(),
        artifacts: vec![Artifact {
            name: format!("{stem}.mmi"),
            text: core::serialize_mmi(score),
            game_parts: game.parts,
            tempo_note_splits: game.tempo_note_splits,
        }],
        split: None,
    })
}

/// 편곡 결과와 저장할 MMI 파일 목록을 만듭니다.
pub fn arrange(
    score: &Score,
    options: &FoldOptions,
    stem: &str,
    track_files: bool,
) -> Result<WorkResult> {
    let result = fold::fold_score(score, options)?;
    let game = game_export(&result.score)?;
    let report = result.report;
    let mut artifacts = vec![Artifact {
        name: format!("{stem}_folded.mmi"),
        text: core::serialize_mmi(&result.score),
        game_parts: game.parts.clone(),
        tempo_note_splits: game.tempo_note_splits,
    }];
    if track_files {
        for (i, track) in result.score.tracks.iter().enumerate() {
            let mut single = result.score.clone();
            single.tracks = vec![track.clone()];
            single.tracks[0].meta.insert("name".into(), "Track1".into());
            let single_game = game_export(&single)?;
            artifacts.push(Artifact {
                name: format!("{stem}_folded_t{}.mmi", i + 1),
                text: core::serialize_mmi(&single),
                game_parts: single_game.parts,
                tempo_note_splits: single_game.tempo_note_splits,
            });
        }
    }
    Ok(WorkResult {
        kind: WorkKind::Fold,
        score: result.score,
        report,
        artifacts,
        split: None,
    })
}

/// 악보를 분할하고 각 장의 MMI 파일 목록을 만듭니다.
pub fn scrolls(score: &Score, options: &SplitOptions, stem: &str) -> Result<WorkResult> {
    let result = split::split_score_python(score, options).or_else(|reference_error| {
        // Python 호환 탐색이 표현 불가능한 조각에서 실패할 때만 무손실 분할로 대체합니다.
        let mut result = split::split_score(score, options)
            .with_context(|| format!("Python split could not encode this input: {reference_error:#}"))?;
        result.report.push_str("\nPython 방식에서 표현할 수 없는 짧은 구간이 있어, 음표를 보존하는 절단점을 사용했습니다.\n");
        Ok::<_, anyhow::Error>(result)
    })?;
    let preview = SplitPreview {
        limit: options.limit,
        chunks: result
            .chunks
            .iter()
            .map(|chunk| SplitChunkPreview {
                start: chunk.start,
                end: chunk.end,
                part_widths: chunk
                    .game_parts
                    .iter()
                    .map(|track| track.iter().map(|part| part.chars().count()).collect())
                    .collect(),
            })
            .collect(),
    };
    let mut artifacts = Vec::new();
    for (i, chunk) in result.chunks.iter().enumerate() {
        artifacts.push(Artifact {
            name: format!("{stem}_{:02}.mmi", i + 1),
            text: core::serialize_mmi(&chunk.score),
            game_parts: chunk.game_parts.clone(),
            tempo_note_splits: chunk.tempo_note_splits,
        });
    }
    let report = result.report;
    Ok(WorkResult {
        kind: WorkKind::Split,
        score: score.clone(),
        report,
        artifacts,
        split: Some(preview),
    })
}

/// 두 악보를 비교한 작업 결과를 만듭니다.
pub fn compare(source: &Score, output: Score, stem: &str) -> WorkResult {
    let report = crate::verify::verify_scores(source, &output);
    let artifacts = vec![Artifact {
        name: format!("{stem}_verification.txt"),
        text: report.clone(),
        game_parts: Vec::new(),
        tempo_note_splits: 0,
    }];
    WorkResult {
        kind: WorkKind::Verify,
        score: output,
        report,
        artifacts,
        split: None,
    }
}

/// 존재하지 않는 대상도 비교할 수 있도록 파일 경로를 정규화합니다.
fn path_identity(path: &Path) -> Result<PathBuf> {
    let mut ancestor = std::path::absolute(path)?;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .context("출력 파일 경로를 확인할 수 없습니다")?
                .to_os_string(),
        );
        if !ancestor.pop() {
            bail!("출력 경로의 상위 폴더를 확인할 수 없습니다");
        }
    }
    let mut resolved = ancestor.canonicalize()?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    #[cfg(windows)]
    {
        Ok(PathBuf::from(resolved.to_string_lossy().to_lowercase()))
    }
    #[cfg(not(windows))]
    {
        Ok(resolved)
    }
}

/// 모든 대상을 먼저 검사한 뒤 덮어쓰기 설정에 따라 파일을 저장합니다.
pub fn write_files(
    files: &[(PathBuf, String)],
    overwrite: bool,
    protected: &[PathBuf],
) -> Result<()> {
    let protected: Vec<_> = protected
        .iter()
        .map(|p| path_identity(p))
        .collect::<Result<_>>()?;
    let mut targets = std::collections::HashSet::new();
    for (path, _) in files {
        if path.is_dir() {
            bail!("출력 경로가 폴더입니다: {}", path.display());
        }
        let identity = path_identity(path)?;
        if protected.contains(&identity) {
            bail!("입력 악보에는 덮어쓸 수 없습니다: {}", path.display());
        }
        if !targets.insert(identity) {
            bail!("출력 파일 경로가 중복됩니다: {}", path.display());
        }
        if path.exists() && !overwrite {
            bail!(
                "파일이 이미 있습니다: {} (다른 경로 또는 --force 사용)",
                path.display()
            );
        }
    }
    for (path, text) in files {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("폴더 생성 실패: {}", parent.display()))?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("파일 저장 실패: {}", path.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("파일 쓰기 실패: {}", path.display()))?;
    }
    Ok(())
}

/// 이름이 겹치지 않는 새 폴더에 작업 결과를 저장합니다.
pub fn export_new_folder(result: &WorkResult, parent: &Path, stem: &str) -> Result<PathBuf> {
    for suffix in 1..=10000 {
        let folder = parent.join(if suffix == 1 {
            format!("{stem}_export")
        } else {
            format!("{stem}_export_{suffix}")
        });
        match fs::create_dir(&folder) {
            Ok(()) => {
                let files = result
                    .artifacts
                    .iter()
                    .map(|a| (folder.join(&a.name), a.text.clone()))
                    .collect::<Vec<_>>();
                write_files(&files, false, &[])?;
                return Ok(folder);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("내보내기 폴더를 만들 수 없습니다"),
        }
    }
    bail!("내보내기 폴더 이름을 만들 수 없습니다")
}
