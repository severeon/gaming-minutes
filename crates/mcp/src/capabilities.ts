/**
 * CLI capabilities feature-detection probe.
 *
 * Phase 2 of #183: instead of guessing which MCP tools to expose based on
 * version strings, ask the CLI directly via `minutes capabilities --json`.
 * Tools whose backing CLI subcommand exists get registered; tools whose
 * backing subcommand is missing (because the CLI pre-dates the feature)
 * are hidden from the MCP tool list entirely.
 *
 * The probe is synchronous so tool registration at module load can consult
 * it. If the CLI is missing, older, or crashes, the probe returns `null`
 * and `hasFeature(null, ...)` returns `true` for every key. That keeps
 * existing behavior intact for old CLIs or missing binaries: register all
 * tools optimistically, let each tool's runtime call surface the error
 * itself. This is strictly better than the alternative (hide tools the
 * CLI actually supports just because we couldn't probe it).
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
  const obj = parsed as Record<string, unknown>;

  if (typeof obj.version !== "string") return null;
  if (typeof obj.api_version !== "number") return null;
  if (!obj.features || typeof obj.features !== "object") return null;

  // Coerce feature map values to booleans; drop non-boolean entries so
  // a misformed payload never accidentally enables a tool.
  const rawFeatures = obj.features as Record<string, unknown>;
  const features: Record<string, boolean> = {};
  for (const [name, value] of Object.entries(rawFeatures)) {
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
 * Contract:
 * - `report === null`: probe failed or CLI is old. Return `true`
 *   (optimistic: register the tool and let runtime handle errors).
 * - `report !== null` and feature key is missing: CLI explicitly does not
 *   support this feature. Return `false` (hide the tool).
 * - `report !== null` and feature key is `true`: Return `true`.
 * - `report !== null` and feature key is `false`: Return `false`.
 */
export function hasFeature(
  report: CapabilityReport | null,
  name: string
): boolean {
  if (report === null) return true;
  return report.features[name] === true;
}
