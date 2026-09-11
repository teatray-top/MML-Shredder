// 악보 계산을 화면과 분리하고 요청을 순서대로 처리합니다.
let runtime;
let pending = Promise.resolve();

// 워커와 동일한 배포 버전의 계산 모듈을 준비합니다.
async function loadRuntime() {
  const moduleUrl = new URL("./pkg/mmlfold.js", import.meta.url);
  const wasmUrl = new URL("./pkg/mmlfold_bg.wasm", import.meta.url);
  moduleUrl.search = self.location.search;
  wasmUrl.search = self.location.search;
  const { default: init, worker_execute } = await import(moduleUrl.href);
  await init({ module_or_path: wasmUrl });
  return worker_execute;
}

self.addEventListener("message", (event) => {
  pending = pending.then(async () => {
    try {
      runtime ??= loadRuntime();
      const execute = await runtime;
      const result = execute(event.data);
      self.postMessage({ ok: true, result });
    } catch (error) {
      self.postMessage({ ok: false, error: String(error) });
    }
  });
});

self.addEventListener("messageerror", () => {
  self.postMessage({ ok: false, error: "작업 데이터를 읽지 못했습니다." });
});
