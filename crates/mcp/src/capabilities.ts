/**
 * CLI capabilities feature-detection probe.
 *
 * Phase 2 of #183: instead of guessing which MCP tools to expose based on
 * version strings, ask the CLI directly via `minutes capabilities --json`.
 * Tools whose backing CLI subcommand the report confirms get registered;
 * tools whose subcommand is missing (or whose feature key is absent, or
 * whose probe failed entirely) are hidden from the MCP tool list.
 *
 * The probe is synchronous so tool registration at module load can consult
 * it. If the CLI is missing, older, or crashes, the probe returns `null`
 * and `hasFeature(null, ...)` returns `false` — fail-closed. For the
 * specific features this module gates (new in 0.14.0), that matches
 * ground truth: a CLI too old to respond to `capabilities` is also too
 * old to have the backing subcommand, so hiding the tool is correct.
 *
 * The alternative (fail-open) was rejected because it produced tools that
 * fail at call time with "unknown subcommand" errors on older CLIs — the
 * exact UX problem #183 Phase 2 is meant to eliminate.
 */

import { execFileSync } from "child_process";

export type CapabilityReport = {
  /** Semver of the CLI, e.g. "0.14.0". */
  version: string;
  /** Wire-contract version. Bumps only on breaking changes. */
  api_version: number;
  /** Feature name to whether the CLI supports it. */
  features: Record<string, boolean>;
};

/**
 * Probe the installed CLI for its capability report. Synchronous so it
 * runs before tool registrations at module load.
 *
 * Returns `null` if:
 * - the binary does not exist on PATH or at the resolved path,
 * - the CLI is too old to have a `capabilities` subcommand,
 * - the output is not valid JSON,
 * - the output does not match the expected shape.
 *
 * A `null` return is a soft signal meaning "proceed optimistically"; the
 * caller should register all tools as if every feature is supported.
 */
export function probeCapabilitiesSync(
  binPath: string,
  options: { timeoutMs?: number } = {}
): CapabilityReport | null {
  const timeoutMs = options.timeoutMs ?? 2000;

  let stdout: string;
  try {
    stdout = execFileSync(binPath, ["capabilities", "--json"], {
      timeout: timeoutMs,
      encoding: "utf-8",
      // Silence stderr so the MCP console stays quiet when the CLI is
      // old (and prints an unknown-subcommand error to stderr).
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    return null;
  }

  return parseCapabilityReport(stdout);
}

/**
 * The newest wire-contract version this MCP server understands. A report
 * whose `api_version` exceeds this value is rejected (treated as null by
 * the caller) so a future breaking CLI schema cannot be silently trusted
 * by an older MCP.
 *
 * Add a new compatibility branch here, don't just bump this number, when
 * the CLI schema changes in a non-additive way.
 */
export const MAX_SUPPORTED_API_VERSION = 1;

/**
 * Parse a capability report JSON payload with shape validation.
 *
 * Exposed separately from the probe so unit tests can exercise the
 * parser without spawning a subprocess.
 */
export function parseCapabilityReport(raw: string): CapabilityReport | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw.trim());
  } catch {
    return null;
  }

  if (!parsed || typeof parsed !== "object") return null;
  // Object.create(null) would be ideal for the target map; we guard
  // against `__proto__`/`constructor`/`prototype` pollution below.
  const obj = parsed as Record<string, unknown>;

  if (typeof obj.version !== "string") return null;
  if (typeof obj.api_version !== "number") return null;

  // Reject reports from a future CLI with a wire contract we do not
  // understand. Treating this as null triggers the fail-closed path so
  // no tools get silently enabled based on a schema we cannot verify.
  if (
    !Number.isInteger(obj.api_version) ||
    obj.api_version < 1 ||
    obj.api_version > MAX_SUPPORTED_API_VERSION
  ) {
    return null;
  }

  if (!obj.features || typeof obj.features !== "object") return null;

  // Coerce feature map values to booleans; drop non-boolean entries so
  // a misformed payload never accidentally enables a tool. Use a
  // null-prototype object so polluted keys (__proto__, constructor,
  // prototype) cannot reach anything via the prototype chain.
  const rawFeatures = obj.features as Record<string, unknown>;
  const features: Record<string, boolean> = Object.create(null);
  for (const [name, value] of Object.entries(rawFeatures)) {
    if (name === "__proto__" || name === "constructor" || name === "prototype") {
      continue;
    }
    if (typeof value === "boolean") {
      features[name] = value;
    }
  }

  return {
    version: obj.version,
    api_version: obj.api_version,
    features,
  };
}

/**
 * Decide whether to expose a feature-gated MCP tool.
 *
 * Fail-closed contract:
 * - `report === null`: probe failed or CLI is old/missing. Return `false`.
 *   The gated tool is hidden. For the Phase 2 gate set (features new in
 *   the same CLI release that introduced the `capabilities` subcommand),
 *   this matches ground truth: an old CLI is missing both the probe and
 *   the backing subcommand.
 * - `report !== null` and feature key is present and `true`: Return `true`.
 * - `report !== null` and feature key is `false` or missing: Return `false`.
 */
export function hasFeature(
  report: CapabilityReport | null,
  name: string
): boolean {
  if (report === null) return false;
  return report.features[name] === true;
}
