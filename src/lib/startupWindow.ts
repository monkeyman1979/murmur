// Preserve hidden startup for completed installations, including the owner's
// existing native Login Item. A fresh Finder launch reaches this same path:
// macOS provides no verified launch-source signal here. Do not show all
// returning users on startup until login and manual launches can be separated
// without breaking the existing silent-login behavior. Reopen of an already
// running app is handled in Rust; unfinished onboarding must open.
export function shouldOpenMainWindow(onboardingCompleted: boolean): boolean {
  return !onboardingCompleted;
}
