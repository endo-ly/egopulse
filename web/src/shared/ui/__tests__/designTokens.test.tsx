import { describe, it, expect } from "vitest";
import appCssSource from "../../../app.css?raw";

const COLOR_TOKENS = [
  "--color-bg",
  "--color-panel",
  "--color-panel-2",
  "--color-panel-hover",
  "--color-text",
  "--color-text-strong",
  "--color-muted",
  "--color-muted-2",
  "--color-accent",
  "--color-accent-2",
  "--color-success",
  "--color-danger",
  "--color-warning",
  "--color-accent-soft",
  "--color-accent-2-soft",
  "--color-danger-soft",
  "--color-success-soft",
  "--color-warning-soft",
  "--color-border",
  "--color-border-strong",
] as const;

const SPACING_TOKENS = [
  "--space-xs",
  "--space-sm",
  "--space-md",
  "--space-base",
  "--space-lg",
  "--space-xl",
  "--space-2xl",
  "--space-3xl",
] as const;

const RADIUS_TOKENS = [
  "--radius-sm",
  "--radius-md",
  "--radius-lg",
  "--radius-xl",
  "--radius-2xl",
  "--radius-full",
] as const;

const ALL_TOKENS = [...COLOR_TOKENS, ...SPACING_TOKENS, ...RADIUS_TOKENS] as const;

describe("design tokens", () => {
  it("design_tokens_are_defined_as_css_variables", () => {
    for (const token of ALL_TOKENS) {
      const pattern = new RegExp(`${token}\\s*:`);
      expect(
        pattern.test(appCssSource),
        `${token} must be defined as a CSS custom property in app.css`,
      ).toBe(true);
    }
  });
});
