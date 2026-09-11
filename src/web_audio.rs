//! 브라우저에서 미리듣기 음원을 필요할 때 내려받아 준비한다.

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// 피아노와 나머지 악기의 음원이 모두 준비되었는지 확인한다.
pub fn ready() -> bool {
    crate::synth::piano::ready() && crate::instruments::ready()
}

/// 첫 미리듣기에 필요한 음원을 내려받고 검증해 메모리에 보관한다.
pub async fn prepare() -> Result<(), String> {
    if !crate::synth::piano::ready() {
        let bytes = fetch("./audio/piano.sf2").await?;
        crate::synth::piano::install(&bytes)
            .map_err(|error| format!("피아노를 불러올 수 없습니다: {error:#}"))?;
    }
    if !crate::instruments::ready() {
        let bytes = fetch("./audio/instruments.sf2").await?;
        crate::instruments::install(&bytes)
            .map_err(|error| format!("가상 악기를 불러올 수 없습니다: {error:#}"))?;
    }
    Ok(())
}

/// 현재 웹 앱 경로에서 음원 바이트를 내려받는다.
async fn fetch(path: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("브라우저 창을 찾을 수 없습니다.")?;
    let response = JsFuture::from(window.fetch_with_str(path))
        .await
        .map_err(|error| format!("가상 악기를 내려받을 수 없습니다: {error:?}"))?
        .dyn_into::<web_sys::Response>()
        .map_err(|_| "가상 악기 서버의 응답을 읽을 수 없습니다.".to_owned())?;
    if !response.ok() {
        return Err(format!(
            "가상 악기를 내려받을 수 없습니다: HTTP {}",
            response.status()
        ));
    }
    let buffer = JsFuture::from(
        response
            .array_buffer()
            .map_err(|error| format!("가상 악기 데이터를 읽을 수 없습니다: {error:?}"))?,
    )
    .await
    .map_err(|error| format!("가상 악기 데이터를 읽을 수 없습니다: {error:?}"))?;
    Ok(Uint8Array::new(&buffer).to_vec())
}
