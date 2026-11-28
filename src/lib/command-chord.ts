import { useCommand } from "../commands/registry";
import { useEffectiveBinding } from "../commands/keybindings";
import { formatKeybinding } from "./keybinding-format";

export function commandBinding(commandId: string): string | undefined {
  return useEffectiveBinding(commandId, useCommand(commandId)?.keybinding);
}

export function commandChord(commandId: string): string {
  return formatKeybinding(commandBinding(commandId));
}
