import { computed, ref, watch } from "vue";

export type ThemeMode = "system" | "light" | "dark";
export type ShellGradient = "sunset" | "forest" | "desert" | "mocha";

const STORAGE_KEY = "murmur:theme";
const SHELL_GRADIENT_STORAGE_KEY = "murmur:shell-gradient";

function loadStoredMode(): ThemeMode {
  if (typeof localStorage === "undefined") return "system";
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "system"
    ? stored
    : "system";
}

function loadStoredShellGradient(): ShellGradient {
  if (typeof localStorage === "undefined") return "sunset";
  const stored = localStorage.getItem(SHELL_GRADIENT_STORAGE_KEY);
  if (stored === "midnight") return "desert";
  return stored === "sunset" || stored === "forest" || stored === "desert" || stored === "mocha"
    ? stored
    : "sunset";
}

function detectSystemMode(): "light" | "dark" {
  if (typeof window === "undefined") return "light";
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

const themeMode = ref<ThemeMode>(loadStoredMode());
const shellGradient = ref<ShellGradient>(loadStoredShellGradient());
const systemMode = ref<"light" | "dark">(detectSystemMode());

if (typeof window !== "undefined") {
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", (event) => {
      systemMode.value = event.matches ? "dark" : "light";
    });
}

const effectiveMode = computed<"light" | "dark">(() =>
  themeMode.value === "system" ? systemMode.value : themeMode.value,
);

watch(
  effectiveMode,
  (mode) => {
    document.documentElement.classList.toggle("dark", mode === "dark");
  },
  { immediate: true },
);

watch(themeMode, (mode) => {
  if (typeof localStorage !== "undefined") {
    localStorage.setItem(STORAGE_KEY, mode);
  }
});

watch(
  shellGradient,
  (gradient) => {
    document.documentElement.dataset.shellGradient = gradient;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(SHELL_GRADIENT_STORAGE_KEY, gradient);
    }
  },
  { immediate: true },
);

export function useTheme() {
  function setMode(mode: ThemeMode) {
    themeMode.value = mode;
  }

  function setShellGradient(gradient: ShellGradient) {
    shellGradient.value = gradient;
  }

  function cycle() {
    const next: Record<ThemeMode, ThemeMode> = {
      system: "light",
      light: "dark",
      dark: "system",
    };
    themeMode.value = next[themeMode.value];
  }

  return {
    mode: themeMode,
    effectiveMode,
    shellGradient,
    setMode,
    setShellGradient,
    cycle,
  };
}
