#!/usr/bin/env node

/**
 * SessionStart hook: proactive meeting reminder.
 *
 * When a Claude Code session starts, check if the user has a meeting
 * in the next 60 minutes. If so, nudge them to run /minutes prep.
 *
 * Guards against being annoying:
 * - Only fires on startup (not resume/compact/clear)
 * - Only fires if the user has used /minutes prep before (~/.minutes/preps/ exists)
 * - Only fires during business hours (8am-6pm, weekdays)
 * - Can be disabled via ~/.config/minutes/config.toml: [reminders] enabled = false
 *
 * Hook event: SessionStart
 * Matcher: startup
 */

import { existsSync, readFileSync } from "fs";
import { join } from "path";
import { homedir } from "os";

// Only run on startup, not resume/compact/clear
const input = JSON.parse(process.argv[2] || "{}");
const event = input.session_event || input.event || "";

if (event !== "startup") process.exit(0);

// Guard 1: Only nudge if the user has actually used /minutes prep before
const prepsDir = join(homedir(), ".minutes", "preps");
if (!existsSync(prepsDir)) process.exit(0);

// Guard 2: Only fire during business hours (8am-6pm, weekdays)
const now = new Date();
const hour = now.getHours();
const day = now.getDay(); // 0=Sun, 6=Sat
if (day === 0 || day === 6 || hour < 8 || hour >= 18) process.exit(0);

// Guard 3: Check config for opt-out
const configPath = join(homedir(), ".config", "minutes", "config.toml");
if (existsSync(configPath)) {
  try {
    const config = readFileSync(configPath, "utf-8");
    // Simple TOML check — look for reminders.enabled = false
    if (config.includes("enabled = false") && config.includes("[reminders]")) {
      process.exit(0);
    }
  } catch {
    // Config unreadable — continue
  }
}

// Output a reminder for Claude to check the calendar
const output = {
  additionalContext: `Meeting reminder check: The user is an active Minutes user (has used /minutes prep before). If a Google Calendar MCP tool is available (gcal_list_events), check if the user has a meeting in the next 60 minutes. If they do, briefly mention it: "You have [meeting] in [N] minutes. Run /minutes prep to go in prepared." Keep it to one line — don't be pushy. If no calendar tool is available, skip silently. Do NOT mention this check if there are no upcoming meetings.`,
};

console.log(JSON.stringify(output));
