import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import type { HotkeyBinding } from "@/lib/hotkey";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), binding: null as unknown as Ref<HotkeyBinding>, permissions: null as unknown as Ref<PermissionSnapshot> }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("@/composables/useHotkeyBinding", () => ({ useHotkeyBinding: () => native }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => native }));
const handlers = new Map<string, Set<(event: { payload: unknown }) => void>>();
let wrappers: VueWrapper[] = [];
let wrapper: VueWrapper;
function send(name: string, payload: unknown) { for (const handler of handlers.get(`murmur://${name}`) ?? []) handler({ payload }); }
beforeEach(() => {
  vi.resetModules(); vi.resetAllMocks(); handlers.clear(); wrappers = []; localStorage.clear();
  native.invoke.mockResolvedValue(undefined);
  native.listen.mockImplementation(async (name: string, callback) => {
    if (!handlers.has(name)) handlers.set(name, new Set());
    handlers.get(name)!.add(callback);
    return () => { handlers.get(name)!.delete(callback); };
  });
  native.binding = ref({ kind: "modifier_only", device_mask: 0x10 });
  native.permissions = ref({ microphone: "granted", input_monitoring: "granted", accessibility: "granted" });
});
afterEach(async () => { for (const mounted of wrappers) mounted.unmount(); await flushPromises(); document.body.innerHTML = ""; });
async function render() {
  const Step = (await import("./StepTryIt.vue")).default;
  wrapper = mount(Step, { attachTo: document.body, global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  wrappers.push(wrapper); return wrapper;
}
function activate(attempt_id: number, mode: "hold" | "toggle") { send("activation-mode", { attempt_id, mode }); }
function transcript(attempt_id: number, text = "A real transcript") { send("transcript", { attempt_id, text }); }

function feedbackState() { return wrapper.get('[data-testid="gesture-feedback"]').attributes("data-state"); }
function indicator(state: string) { send("indicator-state", state); }

describe("StepTryIt", () => {
  it.each(["hold", "toggle"] as const)("keeps native %s recording visible through physical key-up and silence until actual stop", async (mode) => {
    await render(); await flushPromises(); await wrapper.setProps({ activePhase: mode });
    expect(feedbackState()).toBe("ready");
    indicator("recording"); send("level", 1); await flushPromises();
    expect(feedbackState()).toBe("ready");
    send("hotkey-binding-state", { pressed: true }); activate(1, mode); await flushPromises();
    expect(feedbackState()).toBe("ready"); // Gesture admission alone is not capture state.
    indicator("recording"); await flushPromises();
    expect(feedbackState()).toBe("recording");
    send("hotkey-binding-state", { pressed: false }); send("level", 0); await flushPromises();
    expect(feedbackState()).toBe("recording");
    expect(wrapper.get('[data-pressed]').attributes("data-pressed")).toBe("false");
    expect(wrapper.get(".inline-recording-dot").attributes("style")).toContain("scale(1)");
    expect(wrapper.find('[data-testid="gesture-input-level"]').exists()).toBe(false);
    expect(wrapper.find('[data-testid="gesture-stop-hint"]').exists()).toBe(false);
    expect(wrapper.get('[data-testid="gesture-feedback"]').text()).toBe("Recording");
    send("recording", false); indicator("transcribing"); await flushPromises();
    expect(feedbackState()).toBe("transcribing");
    expect(wrapper.find('[data-testid="gesture-input-level"]').exists()).toBe(false);
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(wrapper.emitted("toggleComplete")).toBeUndefined();
    transcript(1); await flushPromises(); await wrapper.setProps(mode === "hold" ? { holdComplete: true } : { toggleComplete: true });
    expect(feedbackState()).toBe("complete");
    expect(wrapper.findAll('[data-testid="gesture-success"]')).toHaveLength(1);
    const feedback = wrapper.get('[data-testid="gesture-feedback"]');
    expect(feedback.element.nextElementSibling).toBe(wrapper.get("textarea").element);
    expect(feedback.find('[data-testid="gesture-success"]').exists()).toBe(true);
    for (const state of ["idle", "recording", "transcribing", "microphone-error"]) indicator(state);
    send("level", 1); await flushPromises();
    expect(feedbackState()).toBe("complete");
  });
  it("clamps the badge dot's decorative levels outside live regions without an extra meter or instructions", async () => {
    native.binding.value = { kind: "key_combo", keycode: 49, required_flags: 0x60000 };
    await render(); await flushPromises(); await wrapper.setProps({ activePhase: "toggle" });
    activate(1, "toggle"); indicator("recording");
    for (const [level, expected] of [[0.4, 40], [2, 100], [-1, 0], [NaN, 0], [Infinity, 0], [-Infinity, 0], ["1", 0]] as const) {
      send("level", level); await flushPromises();
      const dot = wrapper.get(".inline-recording-dot");
      expect(dot.attributes("style")).toContain(`scale(${1 + expected / 100 * 0.6})`);
      expect(dot.element.closest('[role="status"], [aria-live]')).toBeNull();
      expect(wrapper.find('[data-testid="gesture-input-level"]').exists()).toBe(false);
    }
    expect(wrapper.get('[data-testid="gesture-feedback"]').text()).toBe("Recording");
    expect(wrapper.get("#onboarding-gesture-help").text()).toContain("⌃⇧Space");
    await wrapper.get("textarea").setValue("Typed, not dictated");
    expect(feedbackState()).toBe("recording");
    expect(wrapper.emitted("toggleComplete")).toBeUndefined();
  });
  it.each(["hold", "toggle"] as const)("centers one %s status badge without a divider, side meter, or repeated instructions", async (mode) => {
    await render(); await flushPromises(); await wrapper.setProps({ activePhase: mode });
    const feedback = wrapper.get('[data-testid="gesture-feedback"]');
    expect(feedback.classes()).toContain("place-items-center");
    expect(feedback.classes()).toContain("h-20");
    expect(feedback.classes()).not.toContain("border-t");
    expect(feedback.text()).toBe("Ready");
    activate(1, mode);
    for (const state of ["recording", "transcribing"] as const) {
      indicator(state); await flushPromises();
      expect(feedback.findAll("[data-gesture-badge]")).toHaveLength(1);
      expect(feedback.find('[data-testid="gesture-input-level"]').exists()).toBe(false);
      expect(feedback.find('[data-testid="gesture-stop-hint"]').exists()).toBe(false);
      expect(feedback.text()).toBe(en.indicator[state]);
    }
    transcript(1); await flushPromises();
    await wrapper.setProps(mode === "hold" ? { holdComplete: true } : { toggleComplete: true });
    expect(feedback.findAll("[data-gesture-badge]")).toHaveLength(1);
    expect(feedback.text()).toBe("Complete");
  });
  it("does not claim recording during pending setup and removes all added listeners on exit", async () => {
    let finish!: () => void;
    native.invoke.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    await render(); await flushPromises();
    activate(1, "hold"); indicator("recording"); send("level", 1); await flushPromises();
    expect(feedbackState()).toBe("preparing");
    expect(wrapper.get("textarea").attributes("readonly")).toBeDefined();
    finish(); await flushPromises();
    expect(feedbackState()).toBe("ready");
    activate(2, "hold"); indicator("recording"); await flushPromises();
    expect(feedbackState()).toBe("recording");
    wrapper.unmount(); await flushPromises();
    expect([...handlers.values()].every((set) => set.size === 0)).toBe(true);
    indicator("transcribing"); transcript(2); send("level", 1);
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(native.invoke).not.toHaveBeenCalledWith("start_microphone_monitor", expect.anything());
    expect(native.invoke).toHaveBeenLastCalledWith("set_onboarding_demo_active", { active: false });
  });
  it.each(["cancelled", "no-audio", "no-speech", "pipeline-error", "model-missing"])("settles %s honestly and ignores late global events until a fresh attempt", async (outcome) => {
    await render(); await flushPromises(); activate(1, "hold"); indicator("recording");
    send("dictation-attempt-ended", { attempt_id: 1, outcome });
    for (const state of ["idle", "recording", "transcribing", "microphone-error"]) indicator(state);
    send("level", 1); transcript(1); await flushPromises();
    expect(feedbackState()).toBe("error");
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    activate(2, "hold"); indicator("recording"); send("level", 0.5); await flushPromises();
    expect(feedbackState()).toBe("recording");
    send("dictation-attempt-ended", { attempt_id: 1, outcome }); transcript(1); await flushPromises();
    expect(feedbackState()).toBe("recording");
    expect(wrapper.get(".inline-recording-dot").attributes("style")).toContain("scale(1.3)");
    transcript(2, " "); await flushPromises();
    expect(feedbackState()).toBe("error");
    expect(wrapper.text()).toContain(en.onboarding.tryIt.outcomes["no-speech"]);
  });
  it.each(["hold", "toggle"] as const)("shows stopped rather than recording at the %s recording limit", async (mode) => {
    await render(); await flushPromises(); await wrapper.setProps({ activePhase: mode });
    activate(1, mode); indicator("recording"); indicator("recording-limit"); send("level", 1); await flushPromises();
    expect(feedbackState()).toBe("recording-limit");
    expect(wrapper.text()).toContain("Recording stopped");
    expect(wrapper.find('[data-testid="gesture-stop-hint"]').exists()).toBe(false);
    expect(wrapper.find(".inline-recording-dot").exists()).toBe(false);
    expect(wrapper.find('[data-testid="gesture-success"]').exists()).toBe(false);
  });
  it("resets decoration on phase changes and ignores stale proof until the next accepted attempt", async () => {
    await render(); await flushPromises(); activate(1, "hold"); indicator("recording"); send("level", 1);
    await wrapper.setProps({ activePhase: "toggle" });
    indicator("transcribing"); send("level", 1); transcript(1); await flushPromises();
    expect(feedbackState()).toBe("ready");
    activate(2, "toggle"); indicator("microphone-error"); await flushPromises();
    expect(feedbackState()).toBe("microphone-error");
    expect(wrapper.text()).toContain("Mic unavailable");
    expect(wrapper.find('[data-testid="gesture-success"]').exists()).toBe(false);
  });
  it("uses actual binding segments and physical down/up without a timer", async () => {
    native.binding.value = { kind: "key_combo", keycode: 49, required_flags: 0x60000 };
    await render(); await flushPromises();
    expect(wrapper.findAll("kbd").map((key) => key.text())).toEqual(["⌃", "⇧", "Space"]);
    send("hotkey-binding-state", { pressed: true }); await flushPromises();
    expect(wrapper.get('[data-pressed]').attributes("data-pressed")).toBe("true");
    expect(wrapper.findAll(".onboarding-keycap-pressed")).toHaveLength(3);
    send("hotkey-binding-state", { pressed: false }); await flushPromises();
    expect(wrapper.get('[data-pressed]').attributes("data-pressed")).toBe("false");
  });
  it("completes both correlated modes without setting a dictation language", async () => {
    await render(); await flushPromises();
    expect(wrapper.findAll("kbd").map((key) => key.text())).toEqual(["Right ⌘"]);
    activate(1, "hold"); transcript(1); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("A real transcript");
    await wrapper.setProps({ holdComplete: true, activePhase: "toggle" });
    activate(2, "toggle"); transcript(2); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toHaveLength(1);
    wrapper.unmount(); await flushPromises();
    expect(native.invoke).toHaveBeenLastCalledWith("set_onboarding_demo_active", { active: false });
    expect(localStorage.getItem("murmur:dictation-language")).toBeNull();
    expect(native.invoke).not.toHaveBeenCalledWith("set_dictation_language", expect.anything());
  });
  it("clearly labels the second gesture and shows only one success message", async () => {
    await render(); await flushPromises();
    const instruction = wrapper.get('[data-testid="gesture-instruction"]');
    expect(instruction.text()).toContain("Gesture 1 of 2");
    expect(wrapper.get("h2").text()).toBe("Hold to dictate");
    expect(wrapper.find('[data-testid="gesture-success"]').exists()).toBe(false);

    activate(1, "hold"); transcript(1); await flushPromises();
    await wrapper.setProps({ holdComplete: true });
    expect(wrapper.findAll('[data-testid="gesture-success"]')).toHaveLength(1);
    expect(wrapper.get('[data-testid="gesture-success"]').text()).toBe("Complete");

    await wrapper.setProps({ activePhase: "toggle" });
    expect(instruction.attributes("aria-live")).toBe("polite");
    expect(instruction.text()).toContain("Gesture 2 of 2");
    expect(wrapper.get("h2").text()).toBe("Double-tap for hands-free");
    expect(wrapper.get("#onboarding-gesture-help").text()).toBe("Double-tap Right ⌘ to start, then let go and speak. Tap once to stop.");
    expect(wrapper.find('[data-testid="gesture-success"]').exists()).toBe(false);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("");

    activate(2, "toggle"); transcript(2, "Hands-free transcript"); await flushPromises();
    await wrapper.setProps({ toggleComplete: true });
    expect(wrapper.findAll('[data-testid="gesture-success"]')).toHaveLength(1);
    expect(wrapper.get('[data-testid="gesture-feedback"]').text()).toBe("Complete");
    expect(wrapper.findAll(".text-success").filter((node) => node.element.tagName === "DIV")).toHaveLength(1);
    expect(wrapper.text()).not.toMatch(/Speech captured and inserted|You can use Murmur from any app/);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("Hands-free transcript");
  });
  it.each(["hold", "toggle"] as const)("places %s instructions below the keycap in a roomier card", async (activePhase) => {
    await render(); await flushPromises();
    await wrapper.setProps({ activePhase });
    const instruction = wrapper.get('[data-testid="gesture-instruction"]');
    expect(instruction.classes()).toContain("grid-cols-[minmax(0,1fr)_auto]");
    expect(instruction.element.children[0].tagName).toBe("H2");
    const badge = instruction.get('[data-testid="gesture-progress"]');
    expect(badge.classes()).toContain("justify-self-end");
    expect(instruction.element.children[1]).toBe(badge.element);
    expect(instruction.element.children).toHaveLength(2);
    const action = wrapper.get('[data-testid="gesture-action"]');
    const keys = action.get('[data-pressed]');
    const help = action.get("#onboarding-gesture-help");
    expect(keys.element.nextElementSibling).toBe(help.element);
    expect(action.classes()).toContain("gap-8");
    expect(wrapper.get('[data-testid="gesture-card"]').classes()).toContain("min-h-[28rem]");
    expect(help.text()).toBe(en.onboarding.tryIt[`${activePhase}Body`].replace("{binding}", "Right ⌘"));
    expect(wrapper.get("textarea").attributes("aria-describedby")).toBe(help.attributes("id"));
    expect(wrapper.text()).not.toMatch(/Shortcut released|Shortcut pressed|You can change this later in Settings/);
  });
  it("focuses the editable input at startup and immediately after switching to hands-free", async () => {
    await render(); await flushPromises();
    const input = wrapper.get("textarea").element as HTMLTextAreaElement;
    expect(input.readOnly).toBe(false);
    expect(document.activeElement).toBe(input);
    const next = document.createElement("button");
    document.body.appendChild(next);
    next.focus();
    expect(document.activeElement).toBe(next);
    await wrapper.setProps({ holdComplete: true, activePhase: "toggle" });
    await flushPromises();
    expect(wrapper.get("textarea").element).toBe(input);
    expect(document.activeElement).toBe(input);
    expect(input.readOnly).toBe(false);
    expect(input.placeholder).toBe(en.onboarding.tryIt.togglePlaceholder);
    activate(1, "toggle"); transcript(1); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toHaveLength(1);
  });
  it("wrong mode and uncorrelated transcripts never count or fill the field", async () => {
    await render(); await flushPromises();
    send("transcript", { text: "legacy" });
    activate(1, "toggle"); transcript(1); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(wrapper.emitted("toggleComplete")).toBeUndefined();
    expect(wrapper.text()).not.toMatch(/That was hands-free|That was hold-to-talk/);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("");
    activate(2, "hold"); transcript(2); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
  });
  it("ignores hold-to-talk silently during the hands-free exercise without granting credit", async () => {
    await render(); await flushPromises();
    await wrapper.setProps({ activePhase: "toggle", holdComplete: true });
    activate(1, "hold"); transcript(1); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toBeUndefined();
    expect(wrapper.text()).not.toMatch(/That was hands-free|That was hold-to-talk/);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("");
    activate(2, "toggle"); transcript(2); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toHaveLength(1);
  });
  it("does not complete a silent attempt with an empty transcript", async () => {
    await render(); await flushPromises();
    activate(1, "hold"); transcript(1, "   "); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(wrapper.find('[data-testid="gesture-success"]').exists()).toBe(false);
    expect(wrapper.text()).not.toMatch(/That was hands-free|That was hold-to-talk/);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("");
  });
  it("stale/out-of-order results cannot overwrite newer success or cross phase boundaries", async () => {
    await render(); await flushPromises();
    activate(1, "toggle"); activate(2, "hold"); transcript(2, "Newest"); transcript(1, "Old");
    await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("Newest");
    activate(3, "toggle");
    await wrapper.setProps({ holdComplete: true, activePhase: "toggle" });
    transcript(3); activate(2, "toggle"); transcript(2); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toBeUndefined();
    activate(4, "toggle"); transcript(4); await flushPromises();
    expect(wrapper.emitted("toggleComplete")).toHaveLength(1);
  });
  it.each(["cancelled", "no-audio", "no-speech", "pipeline-error"])("clears correlated %s and allows retry without false credit", async (outcome) => {
    await render(); await flushPromises();
    activate(1, "hold"); send("dictation-attempt-ended", { attempt_id: 1, outcome }); transcript(1);
    await flushPromises();
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(wrapper.text()).toContain(en.onboarding.tryIt.outcomes[outcome as keyof typeof en.onboarding.tryIt.outcomes]);
    activate(2, "hold"); transcript(2); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
  });
  it("ignores completion after permissions are revoked", async () => {
    await render(); await flushPromises(); activate(1, "hold");
    native.permissions.value = { ...native.permissions.value, microphone: "denied" };
    transcript(1); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toBeUndefined();
    expect(wrapper.text()).toContain(en.onboarding.tryIt.gate.headline);
  });
  it("disposes late listeners without enabling an unmounted demo", async () => {
    let finish!: (value: () => void) => void;
    const stop = vi.fn(); native.listen.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    await render(); wrapper.unmount(); finish(stop); await flushPromises();
    expect(stop).toHaveBeenCalledOnce();
    expect(native.listen).toHaveBeenCalledTimes(1);
    expect(native.invoke).not.toHaveBeenCalledWith("set_onboarding_demo_active", { active: true });
  });
  it("old delayed activation cleanup cannot disable a new mounted owner", async () => {
    let finish!: () => void;
    native.invoke.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    const first = await render(); await flushPromises(); first.unmount();
    await render(); await flushPromises(); finish(); await flushPromises();
    expect(native.invoke.mock.calls.map(([, args]) => args)).toEqual([{ active: true }, { active: true }]);
    activate(1, "hold"); transcript(1); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
  });
  it("failed native setup cleans listeners/suppression and coalesces rapid Retry clicks", async () => {
    native.invoke.mockRejectedValueOnce(new Error("demo setup"));
    await render(); await flushPromises();
    expect(wrapper.text()).toContain(en.onboarding.tryIt.outcomes["setup-error"]);
    expect([...handlers.values()].every((set) => set.size === 0)).toBe(true);
    expect(native.invoke).toHaveBeenLastCalledWith("set_onboarding_demo_active", { active: false });
    let finish!: () => void;
    native.invoke.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    const retry = wrapper.findAll("button").find((button) => button.text() === "Retry")!;
    void retry.trigger("click"); void retry.trigger("click"); await flushPromises();
    expect(native.invoke.mock.calls.filter(([, args]) => args.active === true)).toHaveLength(2);
    finish(); await flushPromises(); activate(1, "hold"); transcript(1); await flushPromises();
    expect(wrapper.emitted("holdComplete")).toHaveLength(1);
  });
});
