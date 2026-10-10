import { createApp } from "vue";
import { i18n } from "@/i18n";
import "./style.css";

const windowParam = new URLSearchParams(window.location.search).get("window");

if (windowParam === "indicator") {
  // Indicator window stays on its minimal bundle — no router, no main-app
  // imports. The recording state IS the window's visibility. i18n still
  // needs to land here because the pill copy goes through `useI18n()`.
  import("./components/Indicator.vue").then((module) => {
    createApp(module.default).use(i18n).mount("#app");
  });
} else if (windowParam === "meeting-controller") {
  // Meeting controls are isolated from both the routed app and the transient
  // dictation indicator. Rust owns this window's visibility.
  import("./components/captures/MeetingController.vue").then((module) => {
    createApp(module.default).use(i18n).mount("#app");
  });
} else {
  // Main window gets the router + i18n.
  Promise.all([import("./App.vue"), import("./router")]).then(
    ([appModule, routerModule]) => {
      createApp(appModule.default)
        .use(routerModule.router)
        .use(i18n)
        .mount("#app");
    }
  );
}
