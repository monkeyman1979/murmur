import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises } from "@vue/test-utils";
const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
const info = { exists: true, path: "/model", size: 100, modified: 1, env_override: false, download_url: "", approx_download_bytes: 100 };
let exists = true;
let verifyOk = true;
let fail: string | null = null;
beforeEach(() => {
  vi.resetModules(); vi.resetAllMocks(); exists = true; verifyOk = true; fail = null;
  native.listen.mockResolvedValue(vi.fn());
  native.invoke.mockImplementation(async (command) => {
    if (command === fail) throw new Error(`failed ${command}`);
    if (command === "model_info") return { ...info, exists };
    if (command === "verify_model") return { ok: verifyOk, expected: "a", actual: verifyOk ? "a" : "b", bytes_hashed: 100 };
    if (command === "download_model") { exists = true; return "/model"; }
  });
});
async function setup() { return (await import("./useOnboardingModel")).useOnboardingModel(); }

describe("useModelDownload onboarding operational contract", () => {
  it("verifies an existing file and awaits activation without any network", async () => {
    const model = await setup();
    let activate!: () => void;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command, ...args) => command === "set_active_stt" ? new Promise<void>((resolve) => { activate = resolve; }) : original(command, ...args));
    const preparing = model.prepare();
    await flushPromises();
    expect(model.status.value.kind).toBe("ready");
    expect(model.state.value).toBe("activating");
    expect(model.operational.value).toBe(false);
    expect(native.invoke).toHaveBeenCalledWith("verify_model", { id: "whisper-small-multilingual" });
    expect(native.invoke).not.toHaveBeenCalledWith("download_model", expect.anything());
    activate(); await preparing;
    expect(model.operational.value).toBe(true);
  });
  it("requires explicit network start and trusts successful inline download verification", async () => {
    exists = false;
    const model = await setup();
    await model.prepare();
    expect(model.state.value).toBe("missing");
    expect(native.invoke).not.toHaveBeenCalledWith("download_model", expect.anything());
    await model.retry();
    expect(model.state.value).toBe("operational");
    expect(native.invoke).toHaveBeenCalledWith("download_model", { id: "whisper-small-multilingual" });
    expect(native.invoke).not.toHaveBeenCalledWith("verify_model", expect.anything());
  });
  it("blocks checksum mismatch and force re-downloads instead of trusting existence", async () => {
    verifyOk = false;
    const model = await setup(); await model.prepare();
    expect(model.state.value).toBe("mismatch");
    expect(native.invoke).not.toHaveBeenCalledWith("set_active_stt", expect.anything());
    await model.retry();
    expect(model.state.value).toBe("operational");
    expect(native.invoke.mock.calls.filter(([name]) => name === "verify_model")).toHaveLength(1);
  });
  it.each(["model_info", "verify_model", "set_active_stt"])("exposes %s failure and retries only its failed stage", async (command) => {
    fail = command;
    const model = await setup(); await model.prepare();
    expect(model.state.value).toBe("error");
    expect(model.operational.value).toBe(false);
    native.invoke.mockClear(); fail = null;
    await model.retry();
    expect(model.state.value).toBe("operational");
    if (command === "set_active_stt") expect(native.invoke.mock.calls.map(([name]) => name)).toEqual(["set_active_stt"]);
  });
  it("keeps opaque model failures out of rendered status", async () => {
    native.invoke.mockRejectedValue(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    const { useModelDownload } = await import("./useModelDownload");
    const model = useModelDownload("whisper-small-multilingual");
    await expect(model.refresh()).rejects.toThrow();
    expect(model.status.value).toEqual({
      kind: "error",
      message: "Model status is unavailable. Try again.",
    });
    expect(JSON.stringify(model.status.value)).not.toContain("/Users/private");
    expect(JSON.stringify(model.status.value)).not.toContain("SECRET-LICENSE-TOKEN");
  });

  it("handles download failure and coalesces simultaneous preparation and retries", async () => {
    exists = false;
    const model = await setup(); await Promise.all([model.prepare(), model.prepare()]);
    expect(native.invoke.mock.calls.filter(([name]) => name === "model_info")).toHaveLength(1);
    fail = "download_model";
    await Promise.all([model.retry(), model.retry()]);
    expect(model.state.value).toBe("error");
    fail = null; await model.retry();
    expect(model.state.value).toBe("operational");
  });

  it("rejects download failures with fixed copy after setting sanitized state", async () => {
    exists = false;
    fail = "download_model";
    const { useModelDownload } = await import("./useModelDownload");
    const model = useModelDownload("whisper-small-multilingual");
    await expect(model.download()).rejects.toThrow(
      "Download failed. Check your connection and available storage, then retry.",
    );
    expect(model.status.value).toEqual({
      kind: "error",
      message: "Download failed. Check your connection and available storage, then retry.",
    });
    expect(JSON.stringify(model.status.value)).not.toContain("failed download_model");
  });

  it("coalesces simultaneous verifies for the same model id", async () => {
    let finish!: (report: VerifyReport) => void;
    type VerifyReport = { ok: boolean; expected: string; actual: string; bytes_hashed: number };
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((name, ...args) => name === "verify_model"
      ? new Promise<VerifyReport>((resolve) => { finish = resolve; })
      : original(name, ...args));
    const { useModelDownload } = await import("./useModelDownload");
    const first = useModelDownload("whisper-small-multilingual");
    const second = useModelDownload("whisper-small-multilingual");
    const a = first.verify();
    const b = second.verify();
    await flushPromises();
    expect(native.invoke.mock.calls.filter(([name]) => name === "verify_model")).toHaveLength(1);
    finish({ ok: true, expected: "a", actual: "a", bytes_hashed: 100 });
    await expect(Promise.all([a, b])).resolves.toHaveLength(2);
  });

  it("treats the explicit developer override as unverified read-only activation", async () => {
    native.invoke.mockImplementation(async (command) => {
      if (command === "model_info") return { ...info, env_override: true };
      if (command === "set_active_stt") return;
      throw new Error("unexpected command");
    });
    const model = await setup();
    await model.prepare();
    expect(model.state.value).toBe("operational");
    expect(native.invoke).not.toHaveBeenCalledWith("verify_model", expect.anything());
  });
  it("awaits verification listener installation and exposes hashed-byte progress", async () => {
    let progress!: (event: { payload: { model_id: string; hashed: number; total: number; done: boolean } }) => void;
    native.listen.mockImplementation(async (name, callback) => {
      if (name === "murmur://model-verify") progress = callback;
      return vi.fn();
    });
    let finish!: (report: { ok: boolean }) => void;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((name, ...args) => name === "verify_model" ? new Promise((resolve) => { finish = resolve; }) : original(name, ...args));
    const model = await setup(); const preparing = model.prepare(); await flushPromises();
    expect(model.state.value).toBe("verifying");
    progress({ payload: { model_id: "whisper-small-multilingual", hashed: 50, total: 100, done: false } });
    expect(model.verifyPercent.value).toBe(50);
    expect(model.verifyProgress.value).toEqual({ hashed: 50, total: 100 });
    finish({ ok: true }); await preparing;
    expect(model.verifyProgress.value).toBeNull();
  });

  it("invalidates operational proof when a refresh discovers the file missing", async () => {
    const model = await setup(); await model.prepare();
    exists = false; await model.refresh(); await flushPromises();
    expect(model.operational.value).toBe(false);
    expect(model.state.value).toBe("missing");
  });
});
