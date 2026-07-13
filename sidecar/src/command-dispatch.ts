import type { SidecarCommand } from "./protocol.ts";

export type CommandHandler = (command: SidecarCommand) => Promise<void>;
export type CommandErrorHandler = (command: SidecarCommand, error: unknown) => void;

/**
 * Operations that must not block the JSONL reader. Keeping them detached lets
 * cancel, permission, and OAuth replies reach the active session immediately.
 */
export function runsConcurrently(command: SidecarCommand): boolean {
  return command.type === "bash"
    || command.type === "compact"
    || (command.type === "navigate_tree" && (command.summarize === true || command.instructions !== undefined));
}

export async function dispatchCommand(
  command: SidecarCommand,
  handle: CommandHandler,
  onError: CommandErrorHandler,
): Promise<void> {
  const run = async (): Promise<void> => {
    try {
      await handle(command);
    } catch (error) {
      onError(command, error);
    }
  };

  if (runsConcurrently(command)) {
    void run();
    return;
  }
  await run();
}
