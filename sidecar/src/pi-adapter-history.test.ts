import assert from "node:assert/strict";
import test from "node:test";

import { loadedHistory } from "./pi-adapter.ts";

test("loaded history preserves Pi compaction-aware entry order", () => {
  const contextEntries = [
    { type: "message", message: { role: "user", content: "before", timestamp: 1 } },
    { type: "compaction", summary: "summary one", tokensBefore: 90_000 },
    { type: "message", message: { role: "assistant", content: [{ type: "text", text: "after" }], usage: {}, stopReason: "stop", timestamp: 2 } },
    { type: "branch_summary", summary: "branch summary" },
    { type: "compaction", summary: "summary two", tokensBefore: 70_000 },
  ];
  const manager = {
    buildContextEntries: () => contextEntries,
    getEntries: () => contextEntries,
  } as unknown as Parameters<typeof loadedHistory>[1];
  const session = {
    thinkingLevel: "medium",
    model: undefined,
  } as unknown as Parameters<typeof loadedHistory>[0];

  const history = loadedHistory(session, manager);
  assert.deepEqual(
    history.map((event) => event.type),
    [
      "thinking_changed",
      "user_message",
      "compaction_indicator",
      "text_delta",
      "turn_complete",
      "compaction_indicator",
      "compaction_indicator",
    ],
  );
  const indicators = history.filter((event) => event.type === "compaction_indicator");
  assert.deepEqual(indicators, [
    { type: "compaction_indicator", reason: "manual", tokens_before: 90_000 },
    { type: "compaction_indicator", reason: "branch", tokens_before: undefined },
    { type: "compaction_indicator", reason: "manual", tokens_before: 70_000 },
  ]);
});
