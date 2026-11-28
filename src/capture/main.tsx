import { render } from "solid-js/web";
import CaptureWindow, { applySavedTheme } from "./CaptureWindow";
import { resolvePlatform } from "../lib/platform";

// The capture window's entry (ADR-055). It loads none of the main window's
// stores: `architecture/capture-entry.test.ts` holds it to that.

document.documentElement.setAttribute("data-platform", resolvePlatform());
applySavedTheme();

render(() => <CaptureWindow />, document.getElementById("capture")!);
