import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

// The main window renders Home eagerly (it's the landing view and cheap).
// Everything else is lazy so the initial bundle stays lean.
import Home from "@/views/Home.vue";

export function createAppRoutes(): RouteRecordRaw[] {
  return [
    {
      path: "/",
      name: "home",
      component: Home,
      meta: { title: "Home" },
    },

    {
      path: "/captures",
      name: "captures",
      component: () => import("@/views/Captures.vue"),
      meta: { title: "Meetings" },
    },
    {
      path: "/captures/:captureId",
      name: "capture-detail",
      component: () => import("@/views/CaptureDetail.vue"),
      meta: { title: "Meeting" },
    },

    // Dictations live on an SQLite-backed history page (§2.8).
    {
      path: "/history",
      name: "history",
      component: () => import("@/views/History.vue"),
      meta: { title: "Dictations" },
    },

    // Vocabulary (§2.5) and Corrections (§2.6) share one Dictionary
    // destination. Keep the old paths as redirects so bookmarks and
    // in-app links from older builds still land on the right section.
    {
      path: "/dictionary",
      name: "dictionary",
      component: () => import("@/views/Dictionary.vue"),
      meta: { title: "Dictionary" },
    },
    {
      path: "/vocabulary",
      name: "vocabulary",
      redirect: { name: "dictionary", query: { section: "vocabulary" } },
    },
    {
      path: "/corrections",
      name: "corrections",
      redirect: { name: "dictionary", query: { section: "corrections" } },
    },
    // Settings uses a persistent parent layout so switching its tabs only
    // transitions the tab panel. The app-level route transition still runs
    // when entering or leaving Settings as a top-level destination.
    {
      path: "/settings",
      component: () => import("@/views/settings/SettingsLayout.vue"),
      meta: { title: "Settings" },
      children: [
        {
          path: "",
          redirect: { name: "settings-general" },
        },
        {
          path: "general",
          name: "settings-general",
          component: () => import("@/views/settings/General.vue"),
          meta: { title: "Settings" },
        },
        // Speech-model management now lives in General. Keep this redirect so
        // old deep links still land on the right card.
        {
          path: "models",
          name: "settings-models",
          redirect: {
            name: "settings-general",
            query: { section: "speech-model" },
          },
        },
        {
          path: "permissions",
          name: "settings-permissions",
          component: () => import("@/views/settings/Permissions.vue"),
          meta: { title: "Settings" },
        },
        {
          path: "about",
          name: "settings-about",
          component: () => import("@/views/settings/About.vue"),
          meta: { title: "Settings" },
        },
        {
          path: "acknowledgements",
          name: "settings-acknowledgements",
          component: () => import("@/views/settings/Acknowledgements.vue"),
          meta: { title: "Settings" },
        },
      ],
    },

    // Catch-all. If someone deep-links to a hash we don't recognize, send
    // them home rather than render an empty surface.
    {
      path: "/:pathMatch(.*)*",
      redirect: { name: "home" },
    },
  ];
}

const routes = createAppRoutes();

export const router = createRouter({
  history: createWebHashHistory(),
  routes,
});
