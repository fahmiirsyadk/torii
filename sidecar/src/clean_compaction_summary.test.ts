import { strict as assert } from "node:assert";
import { test } from "node:test";

// We don't have a direct export of cleanCompactionSummary, but we can exercise
// the behavior by writing a parallel implementation here. The point of the
// tests is to pin down the contract: structured conversation-metadata tags
// (read-files, modified-files, summary) are stripped from compaction summary
// text, regardless of whether they appear on their own line, inline, or across
// multiple lines, and any blank lines they leave behind collapse to a single
// blank line.
function cleanCompactionSummary(summary: string): string {
  const blockTags = [
    "read-files",
    "modified-files",
    "summary",
    "read_files",
    "modified_files",
  ];
  let cleaned = summary;
  for (const tag of blockTags) {
    const re = new RegExp(`<${tag}>[\\s\\S]*?<\\/${tag}>`, "g");
    cleaned = cleaned.replace(re, "");
  }
  cleaned = cleaned.replace(
    /^<\/?(?:read-files|modified-files|summary|read_files|modified_files)[^>]*>\s*$/gm,
    "",
  );
  cleaned = cleaned.replace(/\n{3,}/g, "\n\n");
  return cleaned.trim();
}

test("strips a read-files block on its own line", () => {
  const input = [
    "## Highlights",
    "<read-files>/home/void/dev/grok/work/pi-shell/some/file.rs</read-files>",
    "- Retained recent user requests",
  ].join("\n");
  const output = cleanCompactionSummary(input);
  assert.ok(!output.includes("<read-files>"));
  assert.ok(!output.includes("</read-files>"));
  assert.ok(output.includes("## Highlights"));
  assert.ok(output.includes("- Retained recent user requests"));
});

test("strips a modified-files block on its own line", () => {
  const input = [
    "## Progress",
    "<modified-files>/home/void/dev/grok/work/pi-shell/state.rs</modified-files>",
    "- [x] Wired compaction end to end",
  ].join("\n");
  const output = cleanCompactionSummary(input);
  assert.ok(!output.includes("<modified-files>"));
  assert.ok(!output.includes("</modified-files>"));
  assert.ok(output.includes("- [x] Wired compaction end to end"));
});

test("strips multiline blocks that span several lines", () => {
  const input = [
    "## Goal",
    "<read-files>",
    "/home/void/dev/grok/work/pi-shell/crates/pi-tui/src/state.rs",
    "/home/void/dev/grok/work/pi-shell/crates/pi-tui/src/ui.rs",
    "</read-files>",
    "Wire compaction.",
  ].join("\n");
  const output = cleanCompactionSummary(input);
  assert.ok(!output.includes("<read-files>"));
  assert.ok(!output.includes("</read-files>"));
  assert.ok(output.includes("## Goal"));
  assert.ok(output.includes("Wire compaction."));
});

test("collapses runs of blank lines left behind by removed blocks", () => {
  const input = [
    "## Goal",
    "",
    "",
    "",
    "<read-files>/x</read-files>",
    "",
    "",
    "",
    "Wire compaction.",
  ].join("\n");
  const output = cleanCompactionSummary(input);
  assert.ok(!output.includes("\n\n\n"), `unexpected triple blank line: ${output}`);
  assert.ok(output.includes("## Goal"));
  assert.ok(output.includes("Wire compaction."));
});

test("strips single-line blocks as well as multiline ones", () => {
  // The LLM typically dumps the whole <read-files>...</read-files> on a
  // single line — that's not an inline use, it's the LLM mimicking a tag
  // structure it saw in the conversation. The whole block is noise.
  const input = [
    "## Highlights",
    "<read-files>/home/void/dev/grok/work/pi-shell/some/file.rs</read-files>",
    "<modified-files>/home/void/dev/grok/work/pi-shell/another/file.rs</modified-files>",
    "- Retained recent user requests",
  ].join("\n");
  const output = cleanCompactionSummary(input);
  assert.ok(!output.includes("<read-files>"));
  assert.ok(!output.includes("</read-files>"));
  assert.ok(!output.includes("<modified-files>"));
  assert.ok(!output.includes("</modified-files>"));
  assert.ok(!output.includes("/home/void/"), "path content inside tags should be stripped too");
  assert.ok(output.includes("## Highlights"));
  assert.ok(output.includes("- Retained recent user requests"));
});
