import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
let wrapper: VueWrapper;
let exists = false;
let verification = true;
let activationFails = false;
const info = { exists: true, path: "/model", size: 100, modified: 1, env_override: false, download_url: "", approx_download_bytes: 100 };
beforeEach(() => {
  vi.resetModules(); vi.resetAllMocks(); exists = false; verification = true; activationFails = false;
  native.listen.mockResolvedValue(vi.fn());
  native.invoke.mockImplementation(async (name) => {
    if (name === "model_info") return { ...info, exists };
    if (name === "verify_model") return { ok: verification };
    if (name === "download_model") exists = true;
    if (name === "set_active_stt" && activationFails) throw new Error("activation");
  });
});
afterEach(() => wrapper?.unmount());
async function render() {
  const Step = (await import("./StepRequiredModel.vue")).default;
  wrapper = mount(Step, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  return (await import("@/composables/useOnboardingModel")).useOnboardingModel();
}

function expectNeutralProgress() {
  const progress = wrapper.get('[data-testid="model-progress"]');
  expect(progress.classes()).toContain("bg-muted");
  expect(progress.classes()).toContain("border-input");
  expect(progress.html()).not.toContain("success");
  expect(progress.find(".bg-foreground").exists()).toBe(true);
  expect(progress.find(".progress-fill").exists()).toBe(false);
  expect(wrapper.find('[data-testid="model-ready"]').exists()).toBe(false);
}

describe("StepRequiredModel", () => {
  it("missing file stays local until the flow explicitly requests download", async () => {
    const model = await render(); await flushPromises();
    expect(wrapper.text()).toContain("Whisper Small Multilingual");
    expect(model.state.value).toBe("missing");
    expect(native.invoke).not.toHaveBeenCalledWith("download_model", expect.anything());
    await model.retry(); await flushPromises();
    const ready = wrapper.get('[data-testid="model-ready"]');
    expect(ready.text()).toBe("Downloaded and ready.");
    expect(ready.classes()).toContain("bg-success-muted/60");
    expect(ready.findAll("p")).toHaveLength(1);
    expect(wrapper.text()).not.toMatch(/Operational|Checksum verified and activated/);
  });
  it("uses neutral progress colors while downloading and waits for completion", async () => {
    let finish!: () => void;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((name, ...args) => name === "download_model"
      ? new Promise<void>((resolve) => { finish = () => { exists = true; resolve(); }; })
      : original(name, ...args));
    const model = await render(); await flushPromises();
    const download = model.retry(); await flushPromises();
    expect(wrapper.text()).toContain("Downloading model");
    expectNeutralProgress();
    expect(model.operational.value).toBe(false);
    finish(); await download; await flushPromises();
    expect(wrapper.get('[data-testid="model-ready"]').text()).toBe("Downloaded and ready.");
  });
  it("renders existing-file verification rather than premature readiness", async () => {
    exists = true;
    let finish!: (value: { ok: boolean }) => void;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((name, ...args) => name === "verify_model" ? new Promise((resolve) => { finish = resolve; }) : original(name, ...args));
    await render(); await flushPromises();
    expect(wrapper.text()).toContain("Verifying");
    expectNeutralProgress();
    expect(wrapper.text()).not.toContain("Downloaded and ready.");
    finish({ ok: true }); await flushPromises();
    expect(wrapper.text()).toContain("Downloaded and ready.");
  });
  it("renders checksum mismatch and recovers through inline-verified re-download", async () => {
    exists = true; verification = false;
    const model = await render(); await flushPromises();
    expect(wrapper.text()).toContain("checksum does not match");
    expect(wrapper.text()).not.toContain("Downloaded and ready.");
    await model.retry(); await flushPromises();
    expect(wrapper.text()).toContain("Downloaded and ready.");
  });
  it("activation failure remains blocking until retry succeeds", async () => {
    exists = true; activationFails = true;
    const model = await render(); await flushPromises();
    expect(wrapper.text()).toContain("Model setup needs attention");
    expect(model.operational.value).toBe(false);
    expect(wrapper.text()).toContain("activation failed");
    expect(wrapper.text()).not.toContain("Downloaded and ready.");
    activationFails = false; await model.retry(); await flushPromises();
    expect(wrapper.text()).toContain("Downloaded and ready.");
  });
});
