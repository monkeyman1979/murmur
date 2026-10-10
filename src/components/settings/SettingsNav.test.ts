import { flushPromises, mount } from "@vue/test-utils";
import { defineComponent, ref } from "vue";
import { createMemoryHistory, createRouter } from "vue-router";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import { describe, expect, it } from "vitest";
import SettingsNav from "./SettingsNav.vue";

describe("SettingsNav deferred header Teleport", () => {
  it.each(["general", "permissions", "about"])("mounts a preloaded %s header after onboarding and removes it cleanly", async (section) => {
    const router = createRouter({ history: createMemoryHistory(), routes: [
      { path: "/settings/:section", component: { template: "<p>Settings fixture</p>" } },
    ] });
    await router.push(`/settings/${section}`);
    const errors: unknown[] = [];
    const shell = defineComponent({
      components: { SettingsNav },
      setup: () => ({ completed: ref(false) }),
      template: `<button @click="completed = !completed">Toggle shell</button>
        <Transition name="page-fade" mode="out-in">
          <div v-if="completed"><div id="page-header-actions" /><SettingsNav /><p>Settings body</p></div>
        </Transition>`,
    });
    const wrapper = mount(shell, { attachTo: document.body, global: {
      plugins: [router, createI18n({ legacy: false, locale: "en", messages: { en, es, fr, ru } })], stubs: { transition: false }, config: { errorHandler: (error) => errors.push(error) },
    } });
    try {
      expect(document.querySelector("#page-header-actions")).toBeNull();
      await wrapper.get("button").trigger("click");
      await flushPromises();
      const header = wrapper.get("#page-header-actions");
      expect(header.get('nav[aria-label="Settings sections"]').text()).toBe("GeneralPermissionsAbout");
      expect(header.get('a[aria-current="page"]').attributes("href")).toBe(`/settings/${section}`);
      const nav = header.get("nav").element;
      await wrapper.get("button").trigger("click");
      await flushPromises();
      await new Promise((resolve) => setTimeout(resolve, 100));
      expect(nav.isConnected).toBe(false);
      expect(wrapper.text()).not.toContain("Settings body");
      expect(errors).toEqual([]);
    } finally {
      wrapper.unmount();
    }
  });
});
