import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import History from "./History.vue";

vi.mock("@/composables/useHistory", () => ({
  useHistory: () => ({
    items: ref([]), total: ref(0), loading: ref(false), hasMore: ref(false),
    searchQuery: ref(""), setSearch: vi.fn(), loadMore: vi.fn(), deleteOne: vi.fn(),
  }),
}));

beforeEach(() => localStorage.clear());

describe("Dictations interface locales", () => {
  it.each([
    ["en", "Search dictations", "No transcripts yet.", "Dismiss dictations information"],
    ["es", "Buscar dictados", "Todavía no hay transcripciones.", "Cerrar información sobre dictados"],
    ["fr", "Rechercher des dictées", "Aucune transcription pour le moment.", "Fermer les informations sur les dictées"],
    ["ru", "Поиск диктовок", "Расшифровок пока нет.", "Закрыть информацию о диктовках"],
  ])("renders %s search, notice and accessible dismissal", (locale, search, empty, dismiss) => {
    const wrapper = mount(History, { global: {
      plugins: [createI18n({ legacy: false, locale, fallbackLocale: "en", messages: { en, es, fr, ru } })],
      stubs: {
        Teleport: true, SearchField: { props: ["label"], template: '<input :aria-label="label" />' },
        RememberCorrectionDialog: true, TranscriptRow: true,
      },
    } });
    expect(wrapper.find(`[aria-label="${search}"]`).exists()).toBe(true);
    expect(wrapper.text()).toContain(empty);
    expect(wrapper.find(`[aria-label="${dismiss}"]`).exists()).toBe(true);
    wrapper.unmount();
  });
});
