import { describe, expect, it } from "vitest";
import { isTextModel } from "@/we2ai/modelKind";

describe("isTextModel", () => {
  it.each([
    [undefined, true],
    [null, true],
    ["", true],
    ["text", true],
    ["image", false],
    ["video", false],
    ["audio", false],
    ["other", false],
    ["hologram", false],
  ])("kind %s -> %s", (kind, expected) => {
    expect(isTextModel({ kind })).toBe(expected);
  });
});
