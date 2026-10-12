import { test, expect } from "bun:test";
import { toolReactionEmoji } from "../src/commands/discord";
import { parseToolCallLine } from "../src/runner";

test("parses the tool name from a tool call line", () => {
  expect(parseToolCallLine("● Read(/tmp/a.txt)")).toBe("Read");
  expect(parseToolCallLine("● Bash(ls -la)")).toBe("Bash");
  expect(parseToolCallLine("● mcp__plugin_discord_discord__reply(...)")).toBe("mcp__plugin_discord_discord__reply");
});

test("ignores tool result lines and other text", () => {
  expect(parseToolCallLine("  ⎿  [Read] done")).toBeNull();
  expect(parseToolCallLine("Read(/tmp/a.txt)")).toBeNull();
  expect(parseToolCallLine("")).toBeNull();
});

test("groups tools into one emoji per category", () => {
  expect(toolReactionEmoji("Read")).toBe(toolReactionEmoji("Grep"));
  expect(toolReactionEmoji("Glob")).toBe("\u{1F4D6}");
  expect(toolReactionEmoji("Edit")).toBe(toolReactionEmoji("Write"));
  expect(toolReactionEmoji("Bash")).toBe("\u{1F4BB}");
  expect(toolReactionEmoji("WebSearch")).toBe(toolReactionEmoji("WebFetch"));
  expect(toolReactionEmoji("Agent")).toBe("\u{1F916}");
  expect(toolReactionEmoji("Skill")).toBe("\u{1F9E9}");
  expect(toolReactionEmoji("mcp__claude_ai_Atlassian__search")).toBe("\u{1F50C}");
  expect(toolReactionEmoji("TodoWrite")).toBe("\u{1F6E0}️");
});
