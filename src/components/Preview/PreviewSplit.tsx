import type { SplitOrientation } from "../../lib/preview-layout";

interface Props {
  orientation: SplitOrientation;
  ratio: number;
  /** The split container, for translating pointer position into a ratio. */
  container: () => HTMLElement | undefined;
  /** Live ratio update during drag (parent applies it to flex-basis). */
  onResize: (ratio: number) => void;
  /** Persist the current ratio (drag end / keyboard commit). */
  onCommit: () => void;
  /** What the handle resizes, for assistive tech; the preview split's when not given. */
  label?: string;
  /** The narrowest and widest ratio; the preview split's when not given. */
  bounds?: { min: number; max: number };
}

const MIN_RATIO = 0.15;
const MAX_RATIO = 0.85;
const KEY_STEP = 0.02;

export default function PreviewSplit(props: Props) {
  let dragging = false;
  const min = () => props.bounds?.min ?? MIN_RATIO;
  const max = () => props.bounds?.max ?? MAX_RATIO;
  const clamp = (r: number) => Math.min(max(), Math.max(min(), r));

  function onPointerDown(e: PointerEvent) {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const el = props.container();
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const raw =
      props.orientation === "vertical"
        ? (e.clientX - rect.left) / rect.width
        : (e.clientY - rect.top) / rect.height;
    props.onResize(clamp(raw));
  }

  function onPointerUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    props.onCommit();
  }

  function onKeyDown(e: KeyboardEvent) {
    const dec = e.key === "ArrowLeft" || e.key === "ArrowUp";
    const inc = e.key === "ArrowRight" || e.key === "ArrowDown";
    if (!dec && !inc) return;
    e.preventDefault();
    props.onResize(clamp(props.ratio + (inc ? KEY_STEP : -KEY_STEP)));
    props.onCommit();
  }

  return (
    <div
      class={`preview-split-handle preview-split-${props.orientation}`}
      role="separator"
      aria-orientation={props.orientation}
      aria-valuenow={Math.round(props.ratio * 100)}
      aria-valuemin={Math.round(min() * 100)}
      aria-valuemax={Math.round(max() * 100)}
      aria-label={props.label ?? "Resize preview split"}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onKeyDown={onKeyDown}
    />
  );
}
