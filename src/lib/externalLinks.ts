import { openUrl } from "@tauri-apps/plugin-opener";

// Public links point to the planned fresh-history repository; verify them after publication.
export const MURMUR_URLS = Object.freeze({
  privacy: "https://github.com/monkeyman1979/murmur/blob/main/docs/PRIVACY.md",
  report: "https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml",
});

// Keep browser launch explicit and limited to reviewed, capability-scoped destinations.
export async function openMurmurLink(destination: keyof typeof MURMUR_URLS): Promise<void> {
  await openUrl(MURMUR_URLS[destination]);
}
