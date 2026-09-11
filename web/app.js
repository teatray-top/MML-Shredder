// 브라우저 실행 파일을 불러오고 편집 화면을 시작합니다.
const canvas = document.getElementById("mml-canvas");
const message = document.getElementById("boot-message");
const retry = document.getElementById("retry");

retry.addEventListener("click", () => window.location.reload());

try {
  const version = new URL(import.meta.url).search;
  const moduleUrl = new URL("./pkg/mmlfold.js", import.meta.url);
  const wasmUrl = new URL("./pkg/mmlfold_bg.wasm", import.meta.url);
  moduleUrl.search = version;
  wasmUrl.search = version;
  const { default: init, start_web } = await import(moduleUrl.href);
  await init({ module_or_path: wasmUrl });
  await start_web(canvas);
  document.documentElement.classList.add("ready");
  document.getElementById("boot").remove();
  canvas.focus();
} catch (error) {
  console.error("MML 세단기 실행 실패", error);
  message.textContent = "실행하지 못했습니다. 새로고침 후 다시 시도해 주세요.";
  retry.hidden = false;
}
