#!/usr/bin/env node

/**
 * Unit tests for post-record.mjs hook.
 *
 * Tests the hook's behavior by simulating different tool inputs
 * and verifying outputs/side-effects.
 *
 * Run: node .claude/plugins/minutes/hooks/test/post-record.test.mjs
 */

import { execFileSync } from "child_process";
import {
  writeFileSync,
  readFileSync,
  mkdirSync,
  rmSync,
  existsSync,
} from "fs";
import { join } from "path";
import { homedir } from "os";
import { tmpdir } from "os";

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`  PASS: ${name}`);
    passed++;
  } catch (e) {
    console.error(`  FAIL: ${name} — ${e.message}`);
    failed++;
  }
}

function assert(condition, msg) {
  if (!condition) throw new Error(msg || "assertion failed");
}

const hookPath = join(
  import.meta.dirname,
  "..",
  "post-record.mjs"
);

function runHook(toolName, command) {
  const input = JSON.stringify({
    tool_name: toolName,
    tool_input: { command },
  });

  try {
    const result = execFileSync("node", [hookPath, input], {
      encoding: "utf-8",
      timeout: 10000,
      env: { ...process.env, HOME: homedir() },
    });
    return { exitCode: 0, stdout: result };
  } catch (e) {
    return { exitCode: e.status || 1, stdout: e.stdout || "", stderr: e.stderr || "" };
  }
}

console.log("\npost-record.mjs hook tests\n");

// Test 1: Hook exits cleanly for non-Bash tools
test("exits cleanly for non-Bash tool", () => {
  const result = runHook("Read", "/some/file");
  // Should exit 0 (early return, no output)
  assert(result.exitCode === 0, `Expected exit 0, got ${result.exitCode}`);
  assert(result.stdout.trim() === "", "Expected no output for non-Bash tool");
});

// Test 2: Hook exits cleanly for non-minutes commands
test("exits cleanly for non-minutes bash command", () => {
  const result = runHook("Bash", "ls -la");
  assert(result.exitCode === 0, `Expected exit 0, got ${result.exitCode}`);
  assert(
    result.stdout.trim() === "",
    "Expected no output for non-minutes command"
  );
});

// Test 3: Hook exits cleanly for minutes commands other than stop/process
test("exits cleanly for minutes search command", () => {
  const result = runHook("Bash", "minutes search pricing");
  assert(result.exitCode === 0, `Expected exit 0, got ${result.exitCode}`);
  assert(
    result.stdout.trim() === "",
    "Expected no output for minutes search"
  );
});

// Test 4: Hook handles missing last-result.json gracefully
test("handles missing last-result.json gracefully", () => {
  // Temporarily rename last-result.json if it exists
  const lastResult = join(homedir(), ".minutes", "last-result.json");
  const backup = lastResult + ".test-backup";
  let hadFile = false;

  if (existsSync(lastResult)) {
    hadFile = true;
    const content = readFileSync(lastResult);
    writeFileSync(backup, content);
    rmSync(lastResult);
  }

  try {
    const result = runHook("Bash", "minutes stop");
    assert(result.exitCode === 0, `Expected exit 0, got ${result.exitCode}`);
  } finally {
    // Restore
    if (hadFile) {
      const content = readFileSync(backup);
      writeFileSync(lastResult, content);
      rmSync(backup);
    }
  }
});

// Test 5: Hook includes next-skill nudge when last-result.json points to a valid meeting
test("includes debrief/tag nudge for valid meeting", () => {
  const lastResult = join(homedir(), ".minutes", "last-result.json");
  const backup = lastResult + ".test-backup-nudge";
  let hadFile = false;

  if (existsSync(lastResult)) {
    hadFile = true;
    writeFileSync(backup, readFileSync(lastResult));
  }

  // Create a temp meeting file with minimal frontmatter
  const tmpMeeting = join(tmpdir(), `minutes-test-nudge-${Date.now()}.md`);
  const meetingContent = `---
title: Test Meeting
date: 2026-04-10T10:00:00
duration: 15m
attendees: [Alice]
---

## Transcript
[ALICE 0:00] This is a test meeting.
`;
  writeFileSync(tmpMeeting, meetingContent, { mode: 0o600 });

  try {
    // Point last-result.json at our temp meeting
    mkdirSync(join(homedir(), ".minutes"), { recursive: true });
    writeFileSync(lastResult, JSON.stringify({ file: tmpMeeting }));

    const result = runHook("Bash", "minutes stop");
    assert(result.exitCode === 0, `Expected exit 0, got ${result.exitCode}`);

    // The output should contain additionalContext with the nudge
    if (result.stdout.trim()) {
      const parsed = JSON.parse(result.stdout.trim());
      assert(
        parsed.additionalContext && parsed.additionalContext.includes("/minutes-debrief"),
        "Expected additionalContext to include /minutes-debrief nudge"
      );
      assert(
        parsed.additionalContext.includes("/minutes-tag"),
        "Expected additionalContext to include /minutes-tag nudge"
      );
    } else {
      // If no output, the nudge should still fire even without alerts
      throw new Error("Expected output with debrief nudge, got empty stdout");
    }
  } finally {
    // Restore — each step in its own try/catch so a cleanup failure
    // doesn't mask the original assertion error or orphan other state.
    try {
      if (hadFile) {
        writeFileSync(lastResult, readFileSync(backup));
        rmSync(backup);
      } else if (existsSync(lastResult)) {
        rmSync(lastResult);
      }
    } catch (cleanupErr) {
      console.error(`  WARN: cleanup failed for last-result.json: ${cleanupErr.message}`);
    }
    try {
      if (existsSync(tmpMeeting)) rmSync(tmpMeeting);
    } catch (cleanupErr) {
      console.error(`  WARN: cleanup failed for tmpMeeting: ${cleanupErr.message}`);
    }
  }
});

console.log(`\n${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);
