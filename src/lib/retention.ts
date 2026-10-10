export type RetentionKind = "dictations" | "meetings";

export function retentionChangeRequiresConfirmation(
  currentDays: number,
  nextDays: number,
): boolean {
  return nextDays > 0 && (currentDays === 0 || nextDays < currentDays);
}

export function retentionUpdatedMessage(
  kind: RetentionKind,
  days: number,
  label: string,
): string {
  const subject = kind === "dictations" ? "Dictations" : "Meeting transcripts";
  return days === 0
    ? `${subject} will be kept forever.`
    : `${subject} will be deleted after ${label}.`;
}
