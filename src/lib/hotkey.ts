// Wire types mirror the Rust enums in hotkey.rs exactly — serde tags
// and snake_case rename must stay in sync.
export type HotkeyBinding =
  | { kind: "modifier_only"; device_mask: number }
  | { kind: "key_combo"; keycode: number; required_flags: number };

// IOKit ev_keymap.h side-aware device bits. Values must match the Rust
// constants in hotkey.rs — these tell us which physical side of a modifier
// is down (plain kCGEventFlagMaskCommand cannot distinguish Left from Right).
export const NX_DEVICELCTLKEYMASK = 0x0001;
export const NX_DEVICELSHIFTKEYMASK = 0x0002;
export const NX_DEVICERSHIFTKEYMASK = 0x0004;
export const NX_DEVICELCMDKEYMASK = 0x0008;
export const NX_DEVICERCMDKEYMASK = 0x0010;
export const NX_DEVICELALTKEYMASK = 0x0020;
export const NX_DEVICERALTKEYMASK = 0x0040;
export const NX_DEVICERCTLKEYMASK = 0x2000;

// CGEventFlags main mask bits used in KeyCombo required_flags.
export const CG_FLAG_SHIFT = 0x20000; // 1 << 17
export const CG_FLAG_CONTROL = 0x40000; // 1 << 18
export const CG_FLAG_ALTERNATE = 0x80000; // 1 << 19  (Option)
export const CG_FLAG_COMMAND = 0x100000; // 1 << 20

export const DEFAULT_BINDING: HotkeyBinding = {
  kind: "modifier_only",
  device_mask: NX_DEVICERCMDKEYMASK,
};

// Maps KeyboardEvent.code for each modifier to the side-aware IOKit bit.
export const MODIFIER_CODE_TO_DEVICE_MASK: Record<string, number> = {
  MetaLeft: NX_DEVICELCMDKEYMASK,
  MetaRight: NX_DEVICERCMDKEYMASK,
  ShiftLeft: NX_DEVICELSHIFTKEYMASK,
  ShiftRight: NX_DEVICERSHIFTKEYMASK,
  ControlLeft: NX_DEVICELCTLKEYMASK,
  ControlRight: NX_DEVICERCTLKEYMASK,
  AltLeft: NX_DEVICELALTKEYMASK,
  AltRight: NX_DEVICERALTKEYMASK,
};

// Carbon / HIToolbox virtual keycodes for non-modifier keys.
// Source of truth: Apple's HIToolbox Events.h, cross-checked against hotkey.rs comment.
export const KEYCODE_FROM_EVENT_CODE: Record<string, number> = {
  // Letters — Carbon layout (not QWERTY order)
  KeyA: 0,
  KeyS: 1,
  KeyD: 2,
  KeyF: 3,
  KeyH: 4,
  KeyG: 5,
  KeyZ: 6,
  KeyX: 7,
  KeyC: 8,
  KeyV: 9,
  KeyB: 11,
  KeyQ: 12,
  KeyW: 13,
  KeyE: 14,
  KeyR: 15,
  KeyY: 16,
  KeyT: 17,
  KeyO: 31,
  KeyU: 32,
  KeyI: 34,
  KeyP: 35,
  KeyL: 37,
  KeyJ: 38,
  KeyK: 40,
  KeyN: 45,
  KeyM: 46,
  // Digits
  Digit1: 18,
  Digit2: 19,
  Digit3: 20,
  Digit4: 21,
  Digit6: 22,
  Digit5: 23,
  Digit9: 25,
  Digit7: 26,
  Digit8: 28,
  Digit0: 29,
  // Named keys
  Enter: 36,
  Tab: 48,
  Space: 49,
  Escape: 53,
  // Arrow keys
  ArrowLeft: 123,
  ArrowRight: 124,
  ArrowDown: 125,
  ArrowUp: 126,
  // F-keys
  F1: 122,
  F2: 120,
  F3: 99,
  F4: 118,
  F5: 96,
  F6: 97,
  F7: 98,
  F8: 100,
  F9: 101,
  F10: 109,
  F11: 103,
  F12: 111,
};

// Reverse map: Carbon keycode → display name, for formatBindingLabel.
const KEYCODE_TO_NAME: Record<number, string> = {};
for (const [code, kc] of Object.entries(KEYCODE_FROM_EVENT_CODE)) {
  // Letter keys
  if (code.startsWith("Key")) {
    KEYCODE_TO_NAME[kc] = code.slice(3); // "KeyA" → "A"
  } else if (code.startsWith("Digit")) {
    KEYCODE_TO_NAME[kc] = code.slice(5); // "Digit1" → "1"
  } else if (code.startsWith("F") && !isNaN(Number(code.slice(1)))) {
    KEYCODE_TO_NAME[kc] = code; // "F1" → "F1"
  } else {
    // Named keys get friendly symbols
    const named: Record<string, string> = {
      Space: "Space",
      Tab: "Tab",
      Enter: "Enter",
      Escape: "Escape",
      ArrowLeft: "←",
      ArrowRight: "→",
      ArrowUp: "↑",
      ArrowDown: "↓",
    };
    if (named[code] !== undefined) KEYCODE_TO_NAME[kc] = named[code];
  }
}

// The side is display copy, not part of the stored IOKit device mask.
const MODIFIER_MASK_TO_PARTS: Record<number, { side: 'left' | 'right'; symbol: string }> = {
  [NX_DEVICELCMDKEYMASK]: { side: 'left', symbol: '⌘' },
  [NX_DEVICERCMDKEYMASK]: { side: 'right', symbol: '⌘' },
  [NX_DEVICELSHIFTKEYMASK]: { side: 'left', symbol: '⇧' },
  [NX_DEVICERSHIFTKEYMASK]: { side: 'right', symbol: '⇧' },
  [NX_DEVICELCTLKEYMASK]: { side: 'left', symbol: '⌃' },
  [NX_DEVICERCTLKEYMASK]: { side: 'right', symbol: '⌃' },
  [NX_DEVICELALTKEYMASK]: { side: 'left', symbol: '⌥' },
  [NX_DEVICERALTKEYMASK]: { side: 'right', symbol: '⌥' },
};
type TranslateHotkey = (key: string) => string;
function modifierLabel(mask: number, translate?: TranslateHotkey): string {
  const parts = MODIFIER_MASK_TO_PARTS[mask];
  if (!parts) return translate?.('hotkeyRecorder.unknown') ?? 'Unknown';
  const side = translate?.(`hotkeyRecorder.${parts.side}`) ?? (parts.side === 'left' ? 'Left' : 'Right');
  return `${side} ${parts.symbol}`;
}

export function formatBindingLabel(binding: HotkeyBinding, translate?: TranslateHotkey): string {
  if (binding.kind === "modifier_only") {
    return modifierLabel(binding.device_mask, translate);
  }

  // KeyCombo: flags in canonical Mac order ⌃ ⌥ ⇧ ⌘, then key name.
  const { keycode, required_flags } = binding;
  let label = "";
  if (required_flags & CG_FLAG_CONTROL) label += "⌃";
  if (required_flags & CG_FLAG_ALTERNATE) label += "⌥";
  if (required_flags & CG_FLAG_SHIFT) label += "⇧";
  if (required_flags & CG_FLAG_COMMAND) label += "⌘";
  label += formatKeycodeName(keycode, translate) ?? `#${keycode}`;
  return label;
}

// Maps modifier event.code to its canonical Mac symbol, for the live
// preview label while the recorder is active.
export function formatModifierSymbol(code: string): string | null {
  switch (code) {
    case "MetaLeft":
    case "MetaRight":
      return "⌘";
    case "ShiftLeft":
    case "ShiftRight":
      return "⇧";
    case "ControlLeft":
    case "ControlRight":
      return "⌃";
    case "AltLeft":
    case "AltRight":
      return "⌥";
    default:
      return null;
  }
}

// Reverse of KEYCODE_FROM_EVENT_CODE — returns the friendly name used in
// formatBindingLabel (e.g. "V", "F5", "Space") for a given Carbon keycode.
export function formatKeycodeName(keycode: number, translate?: TranslateHotkey): string | null {
  const name = KEYCODE_TO_NAME[keycode];
  if (!name) return null;
  const namedKey = ({ Space: 'space', Tab: 'tab', Enter: 'enter', Escape: 'escape' } as Record<string, string>)[name];
  return namedKey && translate ? translate(`hotkeyRecorder.${namedKey}`) : name;
}

// Decompose a binding into visual segments — one per chip. ModifierOnly
// is a single compound chip ("Right ⌘"); KeyCombo splits into one chip
// per required modifier (canonical ⌃ ⌥ ⇧ ⌘ order) plus a trigger chip.
export function bindingSegments(binding: HotkeyBinding, translate?: TranslateHotkey): string[] {
  if (binding.kind === "modifier_only") {
    return [modifierLabel(binding.device_mask, translate)];
  }
  const out: string[] = [];
  if (binding.required_flags & CG_FLAG_CONTROL) out.push("⌃");
  if (binding.required_flags & CG_FLAG_ALTERNATE) out.push("⌥");
  if (binding.required_flags & CG_FLAG_SHIFT) out.push("⇧");
  if (binding.required_flags & CG_FLAG_COMMAND) out.push("⌘");
  out.push(formatKeycodeName(binding.keycode, translate) ?? `#${binding.keycode}`);
  return out;
}

export function isHotkeyBinding(x: unknown): x is HotkeyBinding {
  if (typeof x !== "object" || x === null) return false;
  const obj = x as Record<string, unknown>;
  if (obj.kind === "modifier_only") {
    return typeof obj.device_mask === "number";
  }
  if (obj.kind === "key_combo") {
    return (
      typeof obj.keycode === "number" && typeof obj.required_flags === "number"
    );
  }
  return false;
}
