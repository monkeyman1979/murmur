import { shallowMount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const copyPathMock = vi.hoisted(() => vi.fn());
const localTime = (year: number, month: number, day: number, hour = 12) =>
  new Date(year, month - 1, day, hour).getTime();

vi.mock("@/composables/useCaptures", async () => {
  const { ref } = await import("vue");
  const row = (id: number, startedAt: number, createdAt = startedAt) => ({
    id,
    status: "completed",
    title: `Meeting ${id}`,
    created_at: createdAt,
    started_at: startedAt,
    ended_at: startedAt + 1_000,
    active_duration_ms: 1_000,
    paused_duration_ms: 0,
    source_name: null,
    language: null,
    word_count: 10,
    error_code: null,
    error_message: null,
    markdown_status: "available",
    processing: null,
    has_recovery_audio: false,
    can_retry_transcription: false,
    audio_retention: null,
    audio_delete_at: null,
    audio_bytes: 0,
    audio_kept: false,
  });
  const timestamp = (day: number, hour: number) =>
    new Date(2026, 8, day, hour).getTime();
  const completed = ref([
    row(4, timestamp(5, 10), timestamp(4, 23)),
    row(3, timestamp(5, 8)),
    row(2, timestamp(4, 18)),
    row(1, timestamp(3, 12)),
  ]);
  return {
    useCaptures: () => ({
      summaries: completed,
      needsAttention: ref([]),
      inProgress: ref([]),
      completed,
      query: ref(""),
      loading: ref(false),
      refreshing: ref(false),
      loadingMore: ref(false),
      hasMore: ref(false),
      listError: ref(null),
      stateFor: () => ({ loading: false }),
      refresh: vi.fn(),
      loadMore: vi.fn(),
      setQuery: vi.fn(),
      copyPath: copyPathMock,
    }),
  };
});

vi.mock("@/composables/useMeetingCapture", async () => {
  const { ref } = await import("vue");
  return {
    useMeetingCapture: () => ({
      activeCapture: ref(null),
      startDialogRequested: ref(false),
      consumeStartDialogRequest: vi.fn(),
    }),
  };
});

import Captures from "./Captures.vue";

const stubs = {
  Teleport: { template: "<div><slot /></div>" },
  SearchField: { template: "<div />" },
  StartMeetingDialog: { template: "<div />" },
  CaptureRow: {
    props: ["summary", "allowedActions"],
    emits: ["action"],
    template: '<div data-test="capture-row" :data-id="summary.id" :data-actions="allowedActions.join(\',\')"><button data-test="copy-path" @click="$emit(\'action\', \'copy_path\', summary)">Copy file path</button></div>',
  },
  Badge: {
    template: '<span data-test="date-count"><slot /></span>',
  },
  Button: { template: "<button><slot /></button>" },
  Card: { template: "<section><slot /></section>" },
  CardContent: { template: "<div><slot /></div>" },
  AlertDialog: { template: "<div><slot /></div>" },
  AlertDialogAction: { template: "<button><slot /></button>" },
  AlertDialogCancel: { template: "<button><slot /></button>" },
  AlertDialogContent: { template: "<div><slot /></div>" },
  AlertDialogDescription: { template: "<p><slot /></p>" },
  AlertDialogFooter: { template: "<footer><slot /></footer>" },
  AlertDialogHeader: { template: "<header><slot /></header>" },
  AlertDialogTitle: { template: "<h2><slot /></h2>" },
};

describe("Meetings date groups", () => {
  it.each([
    ["es", "Hoy", "Ayer"], ["fr", "Aujourd’hui", "Hier"], ["ru", "Сегодня", "Вчера"],
  ])("renders %s date bucket headings", (locale, today, yesterday) => {
    vi.setSystemTime(localTime(2026, 9, 5));
    const wrapper = shallowMount(Captures, { global: {
      stubs, plugins: [createI18n({ legacy: false, locale, messages: { en, es, fr, ru } })],
    } });
    const groups = wrapper.findAll('section[aria-labelledby^="meeting-date-"]');
    expect(groups[0].get("h2").text()).toContain(today);
    expect(groups[1].get("h2").text()).toContain(yesterday);
    wrapper.unmount();
  });
  beforeEach(() => {
    copyPathMock.mockReset();
    vi.useFakeTimers();
    vi.setSystemTime(localTime(2026, 9, 5));
  });

  it("groups completed rows by date, exposes file paths, and omits singleton count badges", async () => {
    const wrapper = shallowMount(Captures, { global: { stubs, plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
    const groups = wrapper.findAll('section[aria-labelledby^="meeting-date-"]');
    const olderLabel = new Intl.DateTimeFormat(undefined, {
      year: "numeric",
      month: "long",
      day: "numeric",
    }).format(new Date(localTime(2026, 9, 3)));

    expect(groups).toHaveLength(3);
    expect(groups.map((group) => group.get("h2").text())).toEqual([
      "Today 2",
      "Yesterday",
      olderLabel,
    ]);
    expect(groups[0].findAll('[data-test="date-count"]')).toHaveLength(1);
    expect(groups[0].get('[data-test="date-count"]').attributes("aria-label")).toBe("2 meetings");
    expect(groups[1].findAll('[data-test="date-count"]')).toHaveLength(0);
    expect(groups[2].findAll('[data-test="date-count"]')).toHaveLength(0);
    expect(groups.map((group) =>
      group.findAll('[data-test="capture-row"]').map((row) => row.attributes("data-id")),
    )).toEqual([["4", "3"], ["2"], ["1"]]);
    const firstRow = groups[0].get('[data-test="capture-row"]');
    expect(firstRow.attributes("data-actions")!.split(",")).toContain("copy_path");
    await firstRow.get('[data-test="copy-path"]').trigger("click");
    expect(copyPathMock).toHaveBeenCalledWith(4);
  });
});
