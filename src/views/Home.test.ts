import { config, flushPromises, mount, shallowMount } from "@vue/test-utils";
import { defineComponent, ref, type Ref } from "vue";
import { createMemoryHistory, createRouter } from "vue-router";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const captureMocks = vi.hoisted(() => ({
  copyForAi: vi.fn(),
  saveAudioCopy: vi.fn(),
  deleteAudio: vi.fn(),
  pauseTranscription: vi.fn(),
  resumeTranscription: vi.fn(),
  activeRecording: null as unknown as Ref<{ id: number } | null>,
  meetings: null as unknown as Ref<any[]>,
  completedCount: null as unknown as Ref<number>,
  dictations: null as unknown as Ref<any[]>,
  historyTotal: null as unknown as Ref<number>,
}));

vi.mock("@/lib/hotkey", () => ({ bindingSegments: () => [] }));

vi.mock("@/composables/useHistory", () => ({
  useHistory: () => ({ items: captureMocks.dictations, total: captureMocks.historyTotal, deleteOne: vi.fn() }),
}));

vi.mock("@/composables/useHistoryStats", async () => {
  const { ref } = await import("vue");
  return {
    useHistoryStats: () => ({
      dictatedWords: ref(203_697),
      timeSavedMs: ref(0),
      averageWpm: ref(156),
    }),
  };
});

vi.mock("@/composables/useHotkeyBinding", async () => {
  const { ref } = await import("vue");
  return { useHotkeyBinding: () => ({ binding: ref(null) }) };
});

vi.mock("@/composables/useCaptures", () => {
  return {
    useCaptures: () => ({
      rawSummaries: captureMocks.meetings,
      completedCount: captureMocks.completedCount,
      stateFor: () => ({ loading: false }),
      copyForAi: captureMocks.copyForAi,
      copyTranscript: vi.fn(),
      copyPath: vi.fn(),
      revealTranscript: vi.fn(),
      deleteCapture: vi.fn(),
      deleteAudio: captureMocks.deleteAudio,
      saveAudioCopy: captureMocks.saveAudioCopy,
      cancelTranscription: vi.fn(),
      pauseTranscription: captureMocks.pauseTranscription,
      resumeTranscription: captureMocks.resumeTranscription,
      regenerate: vi.fn(),
      refresh: vi.fn(),
    }),
  };
});

vi.mock("@/composables/useMeetingCapture", () => ({
  useMeetingCapture: () => ({ activeCapture: captureMocks.activeRecording }),
}));

import Home from "./Home.vue";

const stubs = {
  Teleport: { template: "<div><slot /></div>" },
  SpeechModelBanner: { template: "<div />" },
  PermissionsBanner: { template: "<div />" },
  ActiveMeetingCard: { template: "<div />" },
  StatCard: {
    props: ["label"],
    template: '<div data-test="stat-card">{{ label }}</div>',
  },
  CaptureRow: {
    props: ["summary", "allowedActions", "recordingActive"],
    emits: ["action"],
    template: `<div data-test="capture-row" :data-actions="allowedActions.join(',')" :data-recording-active="recordingActive">
      <button data-test="pause-transcription" @click="$emit('action', 'pause_transcription', summary)">Pause transcription</button>
      <button data-test="resume-transcription" @click="$emit('action', 'resume_transcription', summary)">Resume transcription</button>
      <button data-test="copy-for-ai" @click="$emit('action', 'copy_for_ai', summary)">Copy for AI</button>
      <button data-test="save-audio" @click="$emit('action', 'save_audio', summary)">Save audio</button>
      <button data-test="delete-audio" @click="$emit('action', 'delete_audio', summary)">Delete audio</button>
    </div>`,
  },
  Card: { template: "<section><slot /></section>" },
  CardHeader: { template: "<header><slot /></header>" },
  CardTitle: { template: "<h2><slot /></h2>" },
  CardContent: { template: "<div><slot /></div>" },
  RouterLink: { props: ["to"], template: '<a :data-route-name="to.name" :data-section="to.query?.section"><slot /></a>' },
  RememberCorrectionDialog: { template: "<div />" },
  AlertDialog: { template: "<div />" },
};

describe("Home", () => {
  it.each([
    ["es", "Reuniones recientes", "Dictados recientes"],
    ["fr", "Réunions récentes", "Dictées récentes"],
    ["ru", "Недавние встречи", "Недавние диктовки"],
  ])("renders %s core home headings", (locale, meetings, dictations) => {
    config.global.plugins = [createI18n({ legacy: false, locale, messages: { en, es, fr, ru } })];
    const wrapper = shallowMount(Home, { global: { stubs } });
    expect(wrapper.text()).toContain(meetings);
    expect(wrapper.text()).toContain(dictations);
    wrapper.unmount();
  });
  beforeEach(() => { config.global.plugins = [createI18n({ legacy: false, locale: "en", messages: { en } })]; });
  afterEach(() => { config.global.plugins = []; });
  it("defers header Teleport until the post-onboarding shell exists and removes Home on navigation", async () => {
    const router = createRouter({ history: createMemoryHistory(), routes: [
      { path: "/", component: Home },
      { path: "/dictionary", component: { template: "<p>Dictionary fixture body</p>" } },
    ] });
    await router.push("/");
    const shell = defineComponent({
      setup: () => ({ completed: ref(false) }),
      template: `<button @click="completed = true">Finish setup</button>
        <div v-if="completed"><div id="page-header-actions" />
          <RouterView v-slot="{ Component, route }"><Transition mode="out-in">
            <div :key="route.path"><component :is="Component" /></div>
          </Transition></RouterView>
        </div>`,
    });
    const wrapper = mount(shell, { attachTo: document.body, global: {
      plugins: [router], stubs: { ...stubs, Teleport: false, transition: false, RouterView: false, Home: false },
    } });
    await wrapper.get("button").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("Recent dictations");
    expect(wrapper.get("#page-header-actions").text()).not.toBe("");
    const home = wrapper.getComponent(Home).element;
    await router.push("/dictionary");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(wrapper.text()).toContain("Dictionary fixture body");
    expect(wrapper.text()).not.toContain("Recent dictations");
    expect(home.isConnected).toBe(false);
    wrapper.unmount();
  });
  beforeEach(() => {
    localStorage.clear();
    captureMocks.dictations = ref([]);
    captureMocks.historyTotal = ref(0);
    captureMocks.meetings = ref([{ id: 42, status: "completed", title: "Meeting 42" }]);
    captureMocks.completedCount = ref(1);
    captureMocks.activeRecording = ref(null);
    captureMocks.pauseTranscription.mockReset();
    captureMocks.resumeTranscription.mockReset();
    captureMocks.copyForAi.mockReset();
    captureMocks.saveAudioCopy.mockReset();
    captureMocks.deleteAudio.mockReset();
  });

  it.each([0, 1, 2, 3, 4])("shows independent View all links only above three items (count %i)", async (count) => {
    captureMocks.meetings.value = Array.from({ length: count }, (_, id) => ({ id: id + 1, status: "completed", title: `Meeting ${id + 1}` }));
    captureMocks.completedCount.value = count;
    captureMocks.dictations.value = Array.from({ length: count }, (_, id) => ({ id: id + 1, text_final: `Dictation ${id + 1}` }));
    captureMocks.historyTotal.value = count;
    const wrapper = shallowMount(Home, { global: { stubs } });
    expect(wrapper.findAll("a").filter((link) => link.text().includes("View all"))).toHaveLength(count > 3 ? 2 : 0);
    expect(wrapper.findAll('[data-test="capture-row"]')).toHaveLength(Math.min(count, 3));
    wrapper.unmount();
  });

  it("marks older meetings queued while recording and handles manual transcription controls", async () => {
    captureMocks.meetings.value = [{ id: 42, status: "transcribing", title: "Earlier meeting" }];
    captureMocks.activeRecording.value = { id: 43 };
    const wrapper = shallowMount(Home, { global: { stubs } });
    const row = wrapper.get('[data-test="capture-row"]');
    expect(row.attributes("data-recording-active")).toBe("true");
    await row.get('[data-test="pause-transcription"]').trigger("click");
    await row.get('[data-test="resume-transcription"]').trigger("click");
    expect(captureMocks.pauseTranscription).toHaveBeenCalledWith(42);
    expect(captureMocks.resumeTranscription).toHaveBeenCalledWith(42);
    captureMocks.activeRecording.value = null;
    await flushPromises();
    expect(row.attributes("data-recording-active")).toBe("false");
    wrapper.unmount();
  });

  it("shows a one-time closable explanation after the first completed transcript", async () => {
    captureMocks.completedCount.value = 0;
    captureMocks.meetings.value = [{ id: 42, status: "transcribing" }];
    const wrapper = shallowMount(Home, { global: { stubs } });
    expect(wrapper.find("#copy-for-ai-tip").exists()).toBe(false);
    captureMocks.completedCount.value = 1;
    captureMocks.meetings.value = [{ id: 42, status: "completed" }];
    await flushPromises();
    const tip = wrapper.get("#copy-for-ai-tip");
    expect(tip.isVisible()).toBe(true);
    expect(tip.text()).toContain("Murmur does not send it");
    expect(captureMocks.copyForAi).not.toHaveBeenCalled();
    await wrapper.get('[data-test="copy-for-ai"]').trigger("click");
    expect(captureMocks.copyForAi).toHaveBeenCalledWith(42);
    expect(tip.text()).toContain("that service receives it");
    expect(tip.find('a').attributes("data-route-name")).toBe("settings-general");
    expect(tip.find('a').attributes("data-section")).toBe("ai-handoff");
    expect(wrapper.text()).not.toContain("About Copy for AI");
    await tip.get('[aria-label="Dismiss Copy for AI tip"]').trigger("click");
    expect(wrapper.find("#copy-for-ai-tip").exists()).toBe(false);
    expect(localStorage.getItem("murmur:copy-for-ai-tip-seen")).toBe("true");
    await wrapper.get('[data-test="copy-for-ai"]').trigger("click");
    expect(captureMocks.copyForAi).toHaveBeenCalledTimes(2);
    wrapper.unmount();
    const again = shallowMount(Home, { global: { stubs } });
    expect(again.find("#copy-for-ai-tip").exists()).toBe(false);
    again.unmount();
  });

  it("treats the settings link as dismissal so the tip will not reopen on return", async () => {
    const wrapper = shallowMount(Home, { global: { stubs } });
    await wrapper.get('#copy-for-ai-tip a').trigger("click");
    expect(wrapper.find("#copy-for-ai-tip").exists()).toBe(false);
    expect(localStorage.getItem("murmur:copy-for-ai-tip-seen")).toBe("true");
    wrapper.unmount();
  });

  it("reacts to new rows, deletion, and totals beyond the loaded page", async () => {
    const wrapper = shallowMount(Home, { global: { stubs } });
    const links = () => wrapper.findAll("a").filter((link) => link.text().includes("View all"));
    expect(links()).toHaveLength(0);
    captureMocks.completedCount.value = 4; // page still contains only one row
    await flushPromises();
    expect(links()).toHaveLength(1);
    captureMocks.historyTotal.value = 4;
    await flushPromises();
    expect(links()).toHaveLength(2);
    captureMocks.completedCount.value = 1;
    captureMocks.historyTotal.value = 0;
    await flushPromises();
    expect(links()).toHaveLength(0);
    captureMocks.meetings.value = Array.from({ length: 4 }, (_, id) => ({ id, status: "transcribing" }));
    await flushPromises();
    expect(links()).toHaveLength(1); // in-progress rows do not contribute to completedCount
    wrapper.unmount();
  });

  it("uses concise stat copy and exposes the shared meeting action set", async () => {
    const wrapper = shallowMount(Home, { global: { stubs } });

    const statLabels = wrapper.findAll('[data-test="stat-card"]').map((card) => card.text());
    expect(statLabels).toContain("Dictated");
    expect(statLabels).not.toContain("Dictated words");

    const actionsAttribute = wrapper.get('[data-test="capture-row"]').attributes("data-actions");
    expect(actionsAttribute).toBeDefined();
    const actions = (actionsAttribute ?? "").split(",");
    expect(actions).toEqual(expect.arrayContaining([
      "copy_for_ai", "copy_transcript", "copy_path", "reveal",
      "save_audio", "delete_audio", "regenerate", "delete",
    ]));
    expect(actions).not.toContain("open");
    expect(actions).not.toContain("keep_audio");

    await wrapper.get('[data-test="save-audio"]').trigger("click");
    await wrapper.get('[data-test="delete-audio"]').trigger("click");
    expect(captureMocks.saveAudioCopy).toHaveBeenCalledWith(expect.objectContaining({ id: 42 }));
    expect(captureMocks.deleteAudio).not.toHaveBeenCalled();
  });
});
