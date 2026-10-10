import { computed, ref, watch } from "vue";

export const ONBOARDING_STEPS = [
  "welcome",
  "productOverview",
  "permissions",
  "requiredModel",
  "microphone",
  "tryIt",
  "ready",
] as const;

export type StepKey = (typeof ONBOARDING_STEPS)[number];

const COMPLETED_KEY = "murmur:onboarding-completed";
const STEP_KEY = "murmur:onboarding-step-key";
const LEGACY_STEP_KEY = "murmur:onboarding-step";
const LEGACY_STEPS: readonly StepKey[] = [
  "welcome", "permissions", "microphone", "requiredModel", "tryIt", "ready",
];

function loadProgress(): { completed: boolean; step: StepKey } {
  try {
    if (localStorage.getItem(COMPLETED_KEY) === "true") {
      return { completed: true, step: "welcome" };
    }
    const stored = localStorage.getItem(STEP_KEY);
    const legacy = localStorage.getItem(LEGACY_STEP_KEY);
    const step = ONBOARDING_STEPS.find((key) => key === stored)
      ?? (legacy !== null && /^[0-5]$/.test(legacy) ? LEGACY_STEPS[Number(legacy)] : "welcome");
    // Resolve the old order before removing it; model and microphone have swapped.
    try {
      localStorage.setItem(STEP_KEY, step);
      localStorage.removeItem(LEGACY_STEP_KEY);
    } catch {
      // A read-only store should not discard successfully hydrated progress.
    }
    return { completed: false, step };
  } catch {
    // Storage unavailable: setup remains usable for this session.
    return { completed: false, step: "welcome" };
  }
}

const initial = loadProgress();
const completed = ref(initial.completed);
const stepKey = ref<StepKey>(initial.step);
const stepIndex = computed(() => ONBOARDING_STEPS.indexOf(stepKey.value));

watch(stepKey, (step) => {
  if (completed.value) return;
  try {
    localStorage.setItem(STEP_KEY, step);
  } catch {
    // Storage unavailable: retain session progress.
  }
}, { flush: "sync" });

export function useOnboarding() {
  function markComplete(): void {
    completed.value = true;
    try {
      localStorage.setItem(COMPLETED_KEY, "true");
      localStorage.removeItem(LEGACY_STEP_KEY);
      localStorage.removeItem(STEP_KEY);
    } catch {
      // Session-only completion is preferable to blocking entry into the app.
    }
  }

  return { completed, stepKey, stepIndex, markComplete };
}
