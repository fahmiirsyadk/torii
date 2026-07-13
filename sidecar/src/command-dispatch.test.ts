import assert from "node:assert/strict";
import test from "node:test";

import { dispatchCommand, runsConcurrently } from "./command-dispatch.ts";
import type { SidecarCommand } from "./protocol.ts";

const compact: SidecarCommand = {
  type: "compact",
  request_id: "compact-1",
  session_id: "session-1",
};

const cancel: SidecarCommand = {
  type: "cancel",
  request_id: "cancel-1",
  session_id: "session-1",
};

test("long operations detach so control commands remain responsive", async () => {
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  const handled: string[] = [];
  const handle = async (command: SidecarCommand): Promise<void> => {
    handled.push(command.type);
    if (command.type === "compact") await blocked;
  };

  await dispatchCommand(compact, handle, () => assert.fail("unexpected command error"));
  await dispatchCommand(cancel, handle, () => assert.fail("unexpected command error"));

  assert.deepEqual(handled, ["compact", "cancel"]);
  release();
  await blocked;
});

test("only cancellable long operations run concurrently", () => {
  assert.equal(runsConcurrently(compact), true);
  assert.equal(runsConcurrently({ ...cancel }), false);
  assert.equal(runsConcurrently({
    type: "navigate_tree",
    request_id: "tree-1",
    session_id: "session-1",
    entry_id: "entry-1",
    summarize: true,
  }), true);
});
