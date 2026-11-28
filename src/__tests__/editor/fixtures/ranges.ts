import { expect } from "vitest";

export interface Range {
  from: number;
  to: number;
}

/**
 * Passes when both lists hold the same ranges in the same order; otherwise
 * fails on the first range that differs, with both counts. A deep `toEqual`
 * over thousands of ranges costs more than the reading it checks.
 */
export function expectSameRanges(got: readonly Range[], expected: readonly Range[], label: string): void {
  const length = Math.max(got.length, expected.length);
  for (let i = 0; i < length; i++) {
    const a = got[i];
    const b = expected[i];
    if (a !== undefined && b !== undefined && a.from === b.from && a.to === b.to) continue;
    expect({ at: i, count: got.length, from: a?.from ?? null, to: a?.to ?? null }, label).toEqual({
      at: i,
      count: expected.length,
      from: b?.from ?? null,
      to: b?.to ?? null,
    });
    return;
  }
}
