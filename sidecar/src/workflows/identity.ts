import { createHash } from "node:crypto";

function normalize(value: unknown, seen: Set<object>): unknown {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new Error("workflow identity cannot contain non-finite numbers");
    return value;
  }
  if (Array.isArray(value)) return value.map((entry) => normalize(entry, seen));
  if (typeof value === "object") {
    if (seen.has(value)) throw new Error("workflow identity cannot contain cycles");
    seen.add(value);
    const record = value as Record<string, unknown>;
    const normalized: Record<string, unknown> = {};
    for (const key of Object.keys(record).sort()) {
      const entry = record[key];
      if (entry !== undefined) normalized[key] = normalize(entry, seen);
    }
    seen.delete(value);
    return normalized;
  }
  throw new Error(`workflow identity cannot contain ${typeof value}`);
}

export function canonicalJson(value: unknown): string {
  return JSON.stringify(normalize(value, new Set()));
}

export function contentHash(value: unknown): string {
  return createHash("sha256").update(canonicalJson(value)).digest("hex");
}

export function shortHash(value: unknown, length = 16): string {
  return contentHash(value).slice(0, length);
}

export function boundedUntrustedText(value: string, maxBytes: number): string {
  const escaped = value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  const encoded = Buffer.from(escaped, "utf8");
  if (encoded.byteLength <= maxBytes) return escaped;
  return encoded.subarray(0, maxBytes).toString("utf8").replace(/\uFFFD$/, "");
}
