import { invoke } from "@tauri-apps/api/core";

// Owners change synchronously; native mutations run in order across remounts.
// An old step's delayed cleanup must never turn off a newer step's suppression.
let owner: symbol | null = null;
let queue: Promise<unknown> = Promise.resolve();
function serialize(action: () => Promise<unknown>): Promise<unknown> {
  const result = queue.then(action, action);
  queue = result.catch(() => {});
  return result;
}

export function createOnboardingDemoSession() {
  const token = Symbol("onboarding-demo");
  let disposed = false;
  return {
    activate(): Promise<unknown> {
      if (disposed) return Promise.resolve();
      owner = token;
      return serialize(async () => {
        if (!disposed && owner === token) await invoke("set_onboarding_demo_active", { active: true });
      });
    },
    deactivate(): Promise<unknown> {
      if (owner !== token) return Promise.resolve();
      owner = null;
      return serialize(async () => {
        if (owner === null) await invoke("set_onboarding_demo_active", { active: false });
      });
    },
    dispose(): Promise<unknown> {
      disposed = true;
      return this.deactivate();
    },
  };
}
