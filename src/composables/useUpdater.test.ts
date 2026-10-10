import { flushPromises } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  check: vi.fn(),
  invoke: vi.fn(),
  relaunch: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));

function fakeUpdate(version = "1.0.1") {
  return {
    version,
    body: "Bug fixes",
    close: vi.fn().mockResolvedValue(undefined),
    download: vi.fn(async (onEvent: (event: unknown) => void) => {
      onEvent({ event: "Started", data: { contentLength: 100 } });
      onEvent({ event: "Progress", data: { chunkLength: 40 } });
      onEvent({ event: "Progress", data: { chunkLength: 60 } });
      onEvent({ event: "Finished", data: {} });
    }),
    install: vi.fn().mockResolvedValue(undefined),
  };
}

async function loadUpdater({ suppressAutomatic = true } = {}) {
  if (suppressAutomatic) {
    localStorage.setItem("murmur:update-last-check", String(Date.now()));
  }
  Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
  const { useUpdater } = await import("./useUpdater");
  return useUpdater();
}

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  mocks.invoke.mockImplementation(async (command: string) => command === "claim_update_install");
  localStorage.clear();
  delete (window as typeof window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("useUpdater", () => {
  it("checks manually and exposes a version-scoped available update", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    const updater = await loadUpdater();

    await updater.checkForUpdates({ manual: true });

    expect(mocks.check).toHaveBeenCalledWith({ timeout: 15_000 });
    expect(updater.phase.value).toBe("available");
    expect(updater.sidebarVisible.value).toBe(true);
    expect(updater.version.value).toBe("1.0.1");
    expect(updater.notes.value).toBe("Bug fixes");
    updater.dismissUpdate();
    expect(updater.phase.value).toBe("dismissed");
    expect(localStorage.getItem("murmur:update-dismissed-version")).toBe("1.0.1");
  });

  it("reports that a manual check is up to date when no newer release exists", async () => {
    mocks.check.mockResolvedValue(null);
    const updater = await loadUpdater();

    await updater.checkForUpdates({ manual: true });

    expect(updater.phase.value).toBe("up-to-date");
    expect(updater.sidebarVisible.value).toBe(false);
    expect(updater.version.value).toBeNull();
    expect(updater.errorMessage.value).toBeNull();
  });

  it("reports progress, blocks an active-work restart, then installs and relaunches", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });

    await updater.downloadUpdate();
    expect(updater.phase.value).toBe("ready");
    expect(updater.downloadedBytes.value).toBe(100);
    expect(updater.progress.value).toBe(1);

    await updater.installAndRestart(true);
    expect(update.install).not.toHaveBeenCalled();
    await updater.installAndRestart(false);
    expect(update.install).toHaveBeenCalledOnce();
    expect(mocks.relaunch).toHaveBeenCalledOnce();
    expect(mocks.invoke).toHaveBeenCalledOnce();
    expect(mocks.invoke).toHaveBeenCalledWith("claim_update_install");
  });

  it("keeps the downloaded update ready when native work wins the install race", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    mocks.invoke.mockResolvedValueOnce(false);
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });
    await updater.downloadUpdate();

    await updater.installAndRestart(false);

    expect(updater.phase.value).toBe("ready");
    expect(updater.errorMessage.value).toBe("Finish current work, then try again.");
    expect(update.install).not.toHaveBeenCalled();
    expect(mocks.relaunch).not.toHaveBeenCalled();
    expect(mocks.invoke).toHaveBeenCalledOnce();
  });

  it("releases native admission when installation fails before replacing the app", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    update.install.mockRejectedValueOnce(new Error("private installer detail"));
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });
    await updater.downloadUpdate();

    await updater.installAndRestart(false);

    expect(updater.phase.value).toBe("ready");
    expect(updater.errorMessage.value).toBe("Update couldn't be installed — try again.");
    expect(updater.errorSource.value).toBe("install");
    expect(mocks.relaunch).not.toHaveBeenCalled();
    expect(mocks.invoke).toHaveBeenNthCalledWith(1, "claim_update_install");
    expect(mocks.invoke).toHaveBeenNthCalledWith(2, "release_update_install");
  });

  it("keeps native admission closed when restart fails after installation", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    mocks.relaunch.mockRejectedValueOnce(new Error("restart unavailable"));
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });
    await updater.downloadUpdate();

    await updater.installAndRestart(false);

    expect(updater.phase.value).toBe("error");
    expect(updater.errorMessage.value).toBe("Update installed — quit and reopen Murmur.");
    expect(updater.errorSource.value).toBe("restart");
    expect(mocks.invoke).toHaveBeenCalledOnce();
    expect(mocks.invoke).toHaveBeenCalledWith("claim_update_install");

    await updater.retryRelaunch();

    expect(mocks.relaunch).toHaveBeenCalledTimes(2);
    expect(updater.phase.value).toBe("installing");
  });

  it("uses fixed public errors for explicit checks and leaves normal use available", async () => {
    mocks.check.mockRejectedValue(new Error("secret endpoint detail"));
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });
    expect(updater.phase.value).toBe("error");
    expect(updater.errorSource.value).toBe("check");
    expect(updater.sidebarVisible.value).toBe(false);
    expect(updater.errorMessage.value).toBe("Update couldn't be checked — try again.");
    expect(updater.errorMessage.value).not.toContain("secret");
  });

  it("keeps failed background checks quiet and rate-limits the attempt", async () => {
    localStorage.setItem("murmur:update-auto-check", "false"); // ignored legacy preference
    mocks.check.mockRejectedValue(new Error("offline endpoint detail"));
    const updater = await loadUpdater({ suppressAutomatic: false });
    await flushPromises();

    expect(mocks.check).toHaveBeenCalledOnce();
    expect(updater.phase.value).toBe("idle");
    expect(updater.errorMessage.value).toBeNull();
    expect(updater.errorSource.value).toBeNull();
    expect(updater.sidebarVisible.value).toBe(false);
    expect(Number(localStorage.getItem("murmur:update-last-check"))).toBeGreaterThan(0);
  });

  it("keeps a user-initiated download failure available to the update card", async () => {
    const update = fakeUpdate();
    mocks.check.mockResolvedValue(update);
    const updater = await loadUpdater();
    await updater.checkForUpdates({ manual: true });
    update.download.mockRejectedValueOnce(new Error("private download detail"));

    await updater.downloadUpdate();

    expect(updater.phase.value).toBe("error");
    expect(updater.errorSource.value).toBe("download");
    expect(updater.sidebarVisible.value).toBe(true);
    expect(updater.errorMessage.value).toBe("Update couldn't be downloaded — try again.");
    expect(updater.errorMessage.value).not.toContain("private");
  });

  it("does not call the updater API outside an installed Tauri runtime", async () => {
    const { useUpdater } = await import("./useUpdater");
    const updater = useUpdater();
    await updater.checkForUpdates({ manual: true });
    await flushPromises();
    expect(mocks.check).not.toHaveBeenCalled();
    expect(updater.errorMessage.value).toContain("installed Murmur app");
  });
});
