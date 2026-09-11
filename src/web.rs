//! 브라우저 실행, 백그라운드 작업, MMI 다운로드를 연결합니다.

use crate::web_jobs::{JobRequest, JobResponse};
use eframe::egui;
use js_sys::{Array, Uint8Array};
use serde::Deserialize;
use std::{cell::RefCell, collections::HashSet, io::Write, sync::mpsc};
use wasm_bindgen::{JsCast, prelude::*};
use web_sys::{
    Blob, BlobPropertyBag, ErrorEvent, HtmlAnchorElement, HtmlCanvasElement, MessageEvent, Url,
    Worker, WorkerOptions, WorkerType,
};

thread_local! {
    static RUNNER: RefCell<Option<eframe::WebRunner>> = const { RefCell::new(None) };
}

/// 전달받은 캔버스에서 악보 편집 화면을 실행합니다.
#[wasm_bindgen]
pub async fn start_web(canvas: HtmlCanvasElement) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(|cc| Ok(Box::new(crate::gui::MmlApp::new(cc, None)))),
        )
        .await?;
    RUNNER.with(|slot| {
        if let Some(previous) = slot.replace(Some(runner)) {
            previous.destroy();
        }
    });
    Ok(())
}

/// 작업 전용 WASM 인스턴스에서 입력·편곡·분할 요청을 처리합니다.
#[wasm_bindgen]
pub fn worker_execute(request: JsValue) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    let request = serde_wasm_bindgen::from_value(request)
        .map_err(|error| JsValue::from_str(&format!("작업을 읽지 못했습니다: {error}")))?;
    let response = crate::web_jobs::execute(request).map_err(|error| JsValue::from_str(&error))?;
    serde_wasm_bindgen::to_value(&response)
        .map_err(|error| JsValue::from_str(&format!("작업 결과를 전달하지 못했습니다: {error}")))
}

#[derive(Deserialize)]
struct WorkerReply {
    ok: bool,
    result: Option<JobResponse>,
    error: Option<String>,
}

pub struct JobWorker {
    worker: Worker,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(ErrorEvent)>,
    _on_message_error: Closure<dyn FnMut(MessageEvent)>,
}

impl Drop for JobWorker {
    /// 화면에서 작업을 놓으면 워커와 이벤트 수신을 정리합니다.
    fn drop(&mut self) {
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.set_onmessageerror(None);
        self.worker.terminate();
    }
}

/// 새 워커에 작업을 보내고 화면을 갱신할 결과 수신기를 반환합니다.
pub fn start(
    request: JobRequest,
    ctx: egui::Context,
) -> Result<(JobWorker, mpsc::Receiver<Result<JobResponse, String>>), String> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("브라우저 화면을 찾지 못했습니다")?;
    let base = document
        .base_uri()
        .map_err(js_error)?
        .ok_or("작업 파일의 위치를 찾지 못했습니다")?;
    let url = Url::new_with_base("worker.js", &base).map_err(js_error)?;
    let entry = document
        .get_element_by_id("mml-entry")
        .and_then(|element| element.get_attribute("src"))
        .ok_or("작업 파일의 버전을 확인하지 못했습니다. 새로고침해 주세요.")?;
    let entry_url = Url::new_with_base(&entry, &base).map_err(js_error)?;
    url.set_search(&entry_url.search());
    let options = WorkerOptions::new();
    options.set_type(WorkerType::Module);
    let worker = Worker::new_with_options(&url.href(), &options).map_err(js_error)?;
    let (sender, receiver) = mpsc::channel();

    let message_sender = sender.clone();
    let message_ctx = ctx.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let result = serde_wasm_bindgen::from_value::<WorkerReply>(event.data())
            .map_err(|error| format!("작업 결과를 읽지 못했습니다: {error}"))
            .and_then(|reply| {
                if reply.ok {
                    reply
                        .result
                        .ok_or_else(|| "작업 결과가 비어 있습니다".to_owned())
                } else {
                    Err(reply
                        .error
                        .unwrap_or_else(|| "작업을 완료하지 못했습니다".to_owned()))
                }
            });
        let _ = message_sender.send(result);
        message_ctx.request_repaint();
    });

    let error_sender = sender.clone();
    let error_ctx = ctx.clone();
    let on_error = Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
        event.prevent_default();
        let detail = event.message();
        let message = if detail.is_empty() {
            "작업을 시작하지 못했습니다. 새로고침 후 다시 시도해 주세요.".to_owned()
        } else {
            format!("작업을 실행하지 못했습니다: {detail}")
        };
        let _ = error_sender.send(Err(message));
        error_ctx.request_repaint();
    });

    let on_message_error = Closure::<dyn FnMut(MessageEvent)>::new(move |_: MessageEvent| {
        let _ = sender.send(Err("작업 데이터를 전달하지 못했습니다".to_owned()));
        ctx.request_repaint();
    });
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    worker.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
    let job = JobWorker {
        worker,
        _on_message: on_message,
        _on_error: on_error,
        _on_message_error: on_message_error,
    };
    let message = serde_wasm_bindgen::to_value(&request).map_err(|error| error.to_string())?;
    job.worker.post_message(&message).map_err(js_error)?;
    Ok((job, receiver))
}

/// MMI 한 개는 그대로, 여러 개는 ZIP으로 묶어 내려받습니다.
pub fn download(artifacts: &[crate::workflow::Artifact], stem: &str) -> Result<String, String> {
    let files: Vec<_> = artifacts
        .iter()
        .filter(|artifact| artifact.name.to_ascii_lowercase().ends_with(".mmi"))
        .collect();
    if files.is_empty() {
        return Err("저장할 MMI 파일이 없습니다".to_owned());
    }
    if files.len() == 1 {
        let name = safe_filename(&files[0].name, "악보.mmi");
        save_bytes(files[0].text.as_bytes(), &name, "application/octet-stream")?;
        return Ok(format!("{name} 다운로드"));
    }

    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut names = HashSet::new();
    for (index, artifact) in files.iter().enumerate() {
        let mut name = safe_filename(&artifact.name, &format!("악보_{:02}.mmi", index + 1));
        while !names.insert(name.to_lowercase()) {
            let base = name.strip_suffix(".mmi").unwrap_or(&name);
            name = format!("{base}_{:02}.mmi", index + 1);
        }
        archive
            .start_file(name, options)
            .map_err(|error| format!("{}개 파일을 묶지 못했습니다: {error}", files.len()))?;
        archive
            .write_all(artifact.text.as_bytes())
            .map_err(|error| format!("MMI 파일을 저장하지 못했습니다: {error}"))?;
    }
    let bytes = archive
        .finish()
        .map_err(|error| format!("ZIP 파일을 완성하지 못했습니다: {error}"))?
        .into_inner();
    let name = format!("{}.zip", safe_filename(stem, "악보"));
    save_bytes(&bytes, &name, "application/zip")?;
    Ok(format!("{}개 MMI 파일 다운로드", files.len()))
}

/// 경로와 저장할 수 없는 문자를 제거해 내려받기용 파일명을 만듭니다.
fn safe_filename(name: &str, fallback: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = leaf
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']);
    if clean.is_empty() {
        return fallback.to_owned();
    }
    let base = clean
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("_{clean}")
    } else {
        clean.to_owned()
    }
}

/// 메모리의 파일을 브라우저 다운로드로 전달하고 임시 URL을 정리합니다.
fn save_bytes(bytes: &[u8], name: &str, mime: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("브라우저 창을 찾지 못했습니다")?;
    let document = window.document().ok_or("브라우저 화면을 찾지 못했습니다")?;
    let body = document
        .body()
        .ok_or("브라우저 화면이 준비되지 않았습니다")?;
    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .map_err(js_error)?
        .dyn_into()
        .map_err(|error: web_sys::Element| js_error(error.into()))?;
    let parts = Array::new();
    parts.push(&Uint8Array::from(bytes));
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(js_error)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(js_error)?;
    anchor.set_href(&url);
    anchor.set_download(name);
    anchor.set_hidden(true);
    if let Err(error) = body.append_child(&anchor) {
        let _ = Url::revoke_object_url(&url);
        return Err(js_error(error));
    }
    anchor.click();
    anchor.remove();

    // 다운로드가 URL을 읽기 전에 해제되지 않도록 잠시 유지합니다.
    let revoke_url = url.clone();
    let revoke = Closure::once(move || {
        let _ = Url::revoke_object_url(&revoke_url);
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            revoke.as_ref().unchecked_ref(),
            30_000,
        )
        .is_ok()
    {
        revoke.forget();
    } else {
        let _ = Url::revoke_object_url(&url);
    }
    Ok(())
}

/// 브라우저 예외를 사용자에게 표시할 문자열로 바꿉니다.
fn js_error(error: JsValue) -> String {
    error.as_string().unwrap_or_else(|| {
        js_sys::Reflect::get(&error, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
            .unwrap_or_else(|| "브라우저 작업을 완료하지 못했습니다".to_owned())
    })
}
