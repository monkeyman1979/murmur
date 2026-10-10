import { nextTick } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => {
  vi.resetModules();
  localStorage.clear();
  document.documentElement.removeAttribute("data-shell-gradient");
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: vi.fn(() => ({
      matches: false,
      addEventListener: vi.fn(),
    })),
  });
});

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("data-shell-gradient");
});

describe("useTheme shell gradients", () => {
  it("defaults to Sunset Sky and persists changes on the document root", async () => {
    const { useTheme } = await import("./useTheme");
    const theme = useTheme();

    expect(theme.shellGradient.value).toBe("sunset");
    expect(document.documentElement.dataset.shellGradient).toBe("sunset");
    expect(localStorage.getItem("murmur:shell-gradient")).toBe("sunset");

    theme.setShellGradient("desert");
    await nextTick();

    expect(document.documentElement.dataset.shellGradient).toBe("desert");
    expect(localStorage.getItem("murmur:shell-gradient")).toBe("desert");
  });

  it("restores a saved choice and rejects unknown values", async () => {
    localStorage.setItem("murmur:shell-gradient", "forest");
    let themeModule = await import("./useTheme");

    expect(themeModule.useTheme().shellGradient.value).toBe("forest");
    expect(document.documentElement.dataset.shellGradient).toBe("forest");

    vi.resetModules();
    localStorage.setItem("murmur:shell-gradient", "unknown");
    themeModule = await import("./useTheme");

    expect(themeModule.useTheme().shellGradient.value).toBe("sunset");
    expect(document.documentElement.dataset.shellGradient).toBe("sunset");
  });

  it("persists Coffee Mocha as an optional shell gradient without changing the default", async () => {
    const { useTheme } = await import("./useTheme");
    const theme = useTheme();
    expect(theme.shellGradient.value).toBe("sunset");

    theme.setShellGradient("mocha");
    await nextTick();
    expect(document.documentElement.dataset.shellGradient).toBe("mocha");
    expect(localStorage.getItem("murmur:shell-gradient")).toBe("mocha");

    vi.resetModules();
    const restored = await import("./useTheme");
    expect(restored.useTheme().shellGradient.value).toBe("mocha");
  });

  it("migrates the retired Midnight choice to Desert", async () => {
    localStorage.setItem("murmur:shell-gradient", "midnight");
    const { useTheme } = await import("./useTheme");

    expect(useTheme().shellGradient.value).toBe("desert");
    expect(document.documentElement.dataset.shellGradient).toBe("desert");
    expect(localStorage.getItem("murmur:shell-gradient")).toBe("desert");
  });
});
