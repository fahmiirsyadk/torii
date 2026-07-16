import { canonicalJson } from "./identity.ts";
import type { ResolvedWorkflowParameters, WorkflowContractSchema, WorkflowParameterView } from "./types.ts";

export function validateWorkflowValue(value: unknown, schema: WorkflowContractSchema, path: string): void {
  if (schema.type === "object") {
    if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${path} must be an object`);
    const source = value as Record<string, unknown>;
    const unknown = Object.keys(source).filter((key) => schema.properties[key] === undefined);
    if (unknown.length > 0) throw new Error(`${path} contains unsupported fields: ${unknown.join(", ")}`);
    const missing = (schema.required ?? []).filter((key) => source[key] === undefined);
    if (missing.length > 0) throw new Error(`${path} is missing required fields: ${missing.join(", ")}`);
    for (const [key, child] of Object.entries(schema.properties)) {
      if (source[key] !== undefined) validateWorkflowValue(source[key], child, `${path}.${key}`);
    }
    return;
  }
  if (schema.type === "array") {
    if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
    if (value.length > schema.maxItems || value.length < (schema.minItems ?? 0)) throw new Error(`${path} must contain ${schema.minItems ?? 0}-${schema.maxItems} items`);
    value.forEach((entry, index) => validateWorkflowValue(entry, schema.items, `${path}[${index}]`));
    return;
  }
  if (schema.type === "string") {
    if (typeof value !== "string") throw new Error(`${path} must be a string`);
    const length = [...value].length;
    if (length > schema.maxLength || length < (schema.minLength ?? 0)) throw new Error(`${path} length must be ${schema.minLength ?? 0}-${schema.maxLength}`);
    if (schema.enum !== undefined && !schema.enum.includes(value)) throw new Error(`${path} is not an allowed value`);
    return;
  }
  if (schema.type === "number" || schema.type === "integer") {
    if (typeof value !== "number" || !Number.isFinite(value) || (schema.type === "integer" && !Number.isInteger(value))) throw new Error(`${path} must be a finite ${schema.type}`);
    if (schema.minimum !== undefined && value < schema.minimum) throw new Error(`${path} is below minimum ${schema.minimum}`);
    if (schema.maximum !== undefined && value > schema.maximum) throw new Error(`${path} exceeds maximum ${schema.maximum}`);
    if (schema.enum !== undefined && !schema.enum.includes(value)) throw new Error(`${path} is not an allowed value`);
    return;
  }
  if (schema.type === "boolean") {
    if (typeof value !== "boolean") throw new Error(`${path} must be a boolean`);
    if (schema.enum !== undefined && !schema.enum.includes(value)) throw new Error(`${path} is not an allowed value`);
    return;
  }
  if (value !== null) throw new Error(`${path} must be null`);
}

export function normalizeWorkflowParameters(
  configured: ResolvedWorkflowParameters | undefined,
  supplied: unknown,
): Record<string, unknown> {
  if (configured === undefined) {
    if (supplied !== undefined && (typeof supplied !== "object" || supplied === null || Array.isArray(supplied) || Object.keys(supplied as object).length > 0)) {
      throw new Error("workflow does not declare parameters");
    }
    return {};
  }
  if (supplied !== undefined && (typeof supplied !== "object" || supplied === null || Array.isArray(supplied))) {
    throw new Error("workflow parameters must be an object");
  }
  const merged = { ...configured.defaults, ...(supplied as Record<string, unknown> | undefined) };
  validateWorkflowValue(merged, configured.schema, "workflow.parameters");
  const canonical = canonicalJson(merged);
  if (Buffer.byteLength(canonical, "utf8") > configured.maxBytes) throw new Error(`workflow parameters exceed ${configured.maxBytes} bytes`);
  return JSON.parse(canonical) as Record<string, unknown>;
}

export function materializeWorkflowParameterView(
  view: WorkflowParameterView,
  root: Record<string, unknown>,
): Record<string, unknown> {
  const supplied: Record<string, unknown> = {};
  for (const [name, binding] of Object.entries(view.bindings)) {
    if ("literal" in binding) {
      supplied[name] = structuredClone(binding.literal);
      continue;
    }
    let value: unknown = root;
    for (const segment of binding.sourcePath) {
      if (typeof value !== "object" || value === null || Array.isArray(value) || !Object.hasOwn(value, segment)) {
        throw new Error(`workflow parameter view ${view.invocation} cannot resolve source path ${JSON.stringify(binding.sourcePath)}`);
      }
      value = (value as Record<string, unknown>)[segment];
    }
    supplied[name] = structuredClone(value);
  }
  return normalizeWorkflowParameters(view.parameters, supplied);
}
