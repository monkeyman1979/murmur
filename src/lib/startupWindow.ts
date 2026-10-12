// Only automatic login startup stays in the menu bar. A deliberate cold
// launch from Finder/Spotlight must show the window even after onboarding.
// Rust supplies the launch source; the saved login preference is not a
// launch-source signal. Reopen of an already-running app is handled in Rust.
export function shouldOpenMainWindow(
  onboardingCompleted: boolean,
  launchedAtLogin: boolean,
): boolean {
  return !onboardingCompleted || !launchedAtLogin;
}
