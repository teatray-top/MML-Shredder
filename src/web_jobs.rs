//! 웹 작업자의 악보 읽기·편곡·분할 요청을 공통 처리 함수에 연결합니다.

use std::path::Path;

use crate::{
    Score,
    fold::FoldOptions,
    input,
    split::SplitOptions,
    verify,
    workflow::{self, WorkResult},
};

#[derive(serde::Serialize, serde::Deserialize)]
pub enum JobRequest {
    Load {
        name: String,
        bytes: Vec<u8>,
    },
    Arrange {
        score: Score,
        options: FoldOptions,
        stem: String,
        track_files: bool,
    },
    Split {
        score: Score,
        options: SplitOptions,
        stem: String,
    },
    Export {
        score: Score,
        stem: String,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
pub enum JobResponse {
    Loaded {
        name: String,
        score: Score,
        notes: usize,
        polyphony: usize,
    },
    Worked(WorkResult),
}

/// 전달받은 악보 자료형을 그대로 사용해 요청을 수행합니다.
pub fn execute(request: JobRequest) -> Result<JobResponse, String> {
    execute_inner(request).map_err(|error| format!("{error:#}"))
}

/// 요청 종류에 맞는 파일 파서 또는 편곡·분할 함수를 호출합니다.
fn execute_inner(request: JobRequest) -> anyhow::Result<JobResponse> {
    match request {
        JobRequest::Load { name, bytes } => {
            let score = input::parse_score_bytes(Path::new(&name), &bytes)?;
            let notes = score.all_notes();
            let polyphony = verify::max_polyphony(&notes);
            Ok(JobResponse::Loaded {
                name,
                score,
                notes: notes.len(),
                polyphony,
            })
        }
        JobRequest::Arrange {
            score,
            options,
            stem,
            track_files,
        } => workflow::arrange(&score, &options, &stem, track_files).map(JobResponse::Worked),
        JobRequest::Split {
            score,
            options,
            stem,
        } => workflow::scrolls(&score, &options, &stem).map(JobResponse::Worked),
        JobRequest::Export { score, stem } => {
            workflow::source_export(&score, &stem).map(JobResponse::Worked)
        }
    }
}
