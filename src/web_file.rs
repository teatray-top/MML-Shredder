//! 브라우저 파일 선택과 취소를 처리하고 악보 바이트를 읽습니다.

use std::{cell::RefCell, rc::Rc};

use futures_channel::oneshot;
use js_sys::Uint8Array;
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, File, HtmlInputElement};

struct FilePicker {
    input: HtmlInputElement,
    on_change: Closure<dyn FnMut(Event)>,
    on_cancel: Closure<dyn FnMut(Event)>,
}

impl FilePicker {
    /// 새 파일 입력과 선택·취소 결과 수신기를 준비합니다.
    fn new() -> Result<(Self, oneshot::Receiver<Option<File>>), String> {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("브라우저 화면을 찾지 못했습니다")?;
        let body = document
            .body()
            .ok_or("브라우저 화면이 준비되지 않았습니다")?;
        let input: HtmlInputElement = document
            .create_element("input")
            .map_err(js_error)?
            .dyn_into()
            .map_err(|_| "파일 선택 창을 만들지 못했습니다".to_owned())?;
        input.set_type("file");
        input.set_accept(".mid,.midi,.mmi");
        input.set_multiple(false);
        input.set_hidden(true);

        let (sender, receiver) = oneshot::channel();
        let sender = Rc::new(RefCell::new(Some(sender)));
        let change_sender = sender.clone();
        let change_input = input.clone();
        let on_change = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
            let file = change_input.files().and_then(|files| files.get(0));
            if let Some(sender) = change_sender.borrow_mut().take() {
                let _ = sender.send(file);
            }
        });
        let on_cancel = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
            if let Some(sender) = sender.borrow_mut().take() {
                let _ = sender.send(None);
            }
        });
        let picker = Self {
            input,
            on_change,
            on_cancel,
        };
        picker
            .input
            .add_event_listener_with_callback("change", picker.on_change.as_ref().unchecked_ref())
            .map_err(js_error)?;
        picker
            .input
            .add_event_listener_with_callback("cancel", picker.on_cancel.as_ref().unchecked_ref())
            .map_err(js_error)?;
        body.append_child(&picker.input).map_err(js_error)?;
        Ok((picker, receiver))
    }
}

impl Drop for FilePicker {
    /// 선택 완료·취소·오류에서 입력 요소와 이벤트 수신을 해제합니다.
    fn drop(&mut self) {
        let _ = self
            .input
            .remove_event_listener_with_callback("change", self.on_change.as_ref().unchecked_ref());
        let _ = self
            .input
            .remove_event_listener_with_callback("cancel", self.on_cancel.as_ref().unchecked_ref());
        self.input.remove();
    }
}

/// 파일을 고르면 이름과 바이트를 반환하고 취소하면 선택 상태를 비웁니다.
pub async fn pick() -> Result<Option<(String, Vec<u8>)>, String> {
    let (picker, receiver) = FilePicker::new()?;
    picker
        .input
        .show_picker()
        .map_err(|error| format!("파일 선택 창을 열지 못했습니다: {}", js_error(error)))?;
    let file = receiver
        .await
        .map_err(|_| "파일 선택이 중단되었습니다".to_owned())?;
    drop(picker);
    let Some(file) = file else {
        return Ok(None);
    };
    let name = file.name();
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|error| format!("{name} 파일을 읽지 못했습니다: {}", js_error(error)))?;
    Ok(Some((name, Uint8Array::new(&buffer).to_vec())))
}

/// 파일 선택과 읽기 과정에서 발생한 브라우저 오류를 문자열로 바꿉니다.
fn js_error(error: JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| format!("브라우저 파일 작업 실패: {error:?}"))
}
