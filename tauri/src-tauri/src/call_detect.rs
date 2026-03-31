//! Auto-detect video/voice calls and prompt the user to start recording.
//!
//! Detection strategy: poll for known call-app processes that are actively
//! using the microphone. Two signals together (process running + mic active)
//! give high confidence with minimal false positives.
//!
//! Currently macOS-only. The detection functions (`running_process_names`,
//! `is_mic_in_use`) use CoreAudio and `ps`. Windows/Linux would need
//! alternative implementations behind `cfg(target_os)` gates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use minutes_core::config::CallDetectionConfig;
use tauri::Emitter;

fn log_call_detect_event(
    level: &str,
    action: &str,
    app_name: Option<&str>,
    process_name: Option<&str>,
    extra: serde_json::Value,
) {
    minutes_core::logging::append_log(&serde_json::json!({
        "ts": chrono::Local::now().to_rfc3339(),
        "level": level,
        "step": "call_detect",
        "file": "",
        "extra": {
            "action": action,
            "app_name": app_name,
            "process_name": process_name,
            "details": extra,
        }
    }))
    .ok();
}

/// State for the call detection background loop.
pub struct CallDetector {
    config: CallDetectionConfig,
    /// Last observed active call session. We still re-arm on call end/start,
    /// but we also re-notify the same active app after a short interval so
    /// back-to-back meetings and sticky Zoom states don't go silent forever.
    active_call: Mutex<Option<ActiveCallState>>,
}

/// Payload emitted to the frontend when a call is detected.
#[derive(Clone, serde::Serialize)]
pub struct CallDetectedPayload {
    pub app_name: String,
    pub process_name: String,
}

#[derive(Clone)]
struct ActiveCallState {
    process_name: String,
    last_notified_at: Instant,
}

enum DetectionTransition {
    NewSession,
    Reminder,
    Noop,
}

const SAME_APP_REMINDER_SECS: u64 = 20;

impl CallDetector {
    pub fn new(config: CallDetectionConfig) -> Self {
        Self {
            config,
            active_call: Mutex::new(None),
        }
    }

    /// Start the background detection loop. Runs in its own thread.
    pub fn start(
        self: Arc<Self>,
        app: tauri::AppHandle,
        recording: Arc<AtomicBool>,
        _processing: Arc<AtomicBool>,
    ) {
        if !self.config.enabled {
            eprintln!("[call-detect] disabled in config");
            log_call_detect_event(
                "info",
                "disabled",
                None,
                None,
                serde_json::json!({
                    "poll_interval_secs": self.config.poll_interval_secs,
                    "apps": self.config.apps,
                }),
            );
            return;
        }

        let interval = Duration::from_secs(self.config.poll_interval_secs.max(1));
        eprintln!(
            "[call-detect] started — polling every {}s for {:?}",
            interval.as_secs(),
            self.config.apps
        );
        log_call_detect_event(
            "info",
            "started",
            None,
            None,
            serde_json::json!({
                "poll_interval_secs": interval.as_secs(),
                "apps": self.config.apps,
            }),
        );

        std::thread::spawn(move || {
            // Initial delay to let the app finish launching
            std::thread::sleep(Duration::from_secs(5));

            loop {
                std::thread::sleep(interval);

                // Skip only while the mic is already in use.
                if recording.load(Ordering::Relaxed) {
                    continue;
                }

                if let Some((display_name, process_name)) = self.detect_active_call() {
                    match self.note_active_call(&process_name) {
                        DetectionTransition::Noop => {}
                        transition => {
                            let action = match transition {
                                DetectionTransition::NewSession => "detected",
                                DetectionTransition::Reminder => "reminder",
                                DetectionTransition::Noop => unreachable!(),
                            };
                            eprintln!(
                                "[call-detect] {}: {} ({})",
                                action, display_name, process_name
                            );
                            log_call_detect_event(
                                "info",
                                action,
                                Some(&display_name),
                                Some(&process_name),
                                serde_json::json!({
                                    "recording_active": recording.load(Ordering::Relaxed),
                                    "reminder_interval_secs": SAME_APP_REMINDER_SECS,
                                }),
                            );

                            crate::commands::show_user_notification(
                                &app,
                                &format!("{} call detected", display_name),
                                "Open Minutes to start recording",
                            );

                            app.emit(
                                "call:detected",
                                CallDetectedPayload {
                                    app_name: display_name,
                                    process_name,
                                },
                            )
                            .ok();
                        }
                    }
                } else {
                    if let Some(previous) = self.clear_active_call() {
                        log_call_detect_event(
                            "info",
                            "cleared",
                            None,
                            Some(&previous),
                            serde_json::json!({
                                "reason": "no active call detected on current poll"
                            }),
                        );
                    }
                }
            }
        });
    }

    /// Check if any configured call app is running AND the mic is active.
    fn detect_active_call(&self) -> Option<(String, String)> {
        // Check mic first — it's the cheaper signal to short-circuit on
        if !is_mic_in_use() {
            return None;
        }

        let running = running_process_names();

        for config_app in &self.config.apps {
            let config_lower = config_app.to_lowercase();
            // Substring match: "zoom.us" matches process "zoom.us",
            // "Microsoft Teams" matches "Microsoft Teams Helper", etc.
            if running.iter().any(|p| {
                p.to_lowercase().contains(&config_lower) || config_lower.contains(&p.to_lowercase())
            }) {
                let display = display_name_for(config_app);
                return Some((display, config_app.clone()));
            }
        }
        None
    }

    fn note_active_call(&self, process_name: &str) -> DetectionTransition {
        let mut active = self.active_call.lock().unwrap();
        let now = Instant::now();
        match active.as_mut() {
            None => {
                *active = Some(ActiveCallState {
                    process_name: process_name.to_string(),
                    last_notified_at: now,
                });
                DetectionTransition::NewSession
            }
            Some(state) if state.process_name != process_name => {
                *state = ActiveCallState {
                    process_name: process_name.to_string(),
                    last_notified_at: now,
                };
                DetectionTransition::NewSession
            }
            Some(state) => {
                if now.duration_since(state.last_notified_at)
                    >= Duration::from_secs(SAME_APP_REMINDER_SECS)
                {
                    state.last_notified_at = now;
                    DetectionTransition::Reminder
                } else {
                    DetectionTransition::Noop
                }
            }
        }
    }

    fn clear_active_call(&self) -> Option<String> {
        let mut active = self.active_call.lock().unwrap();
        active.take().map(|state| state.process_name)
    }
}

/// Friendly display name for a process name.
fn display_name_for(process: &str) -> String {
    match process {
        "zoom.us" => "Zoom".into(),
        "Microsoft Teams" | "Microsoft Teams (work or school)" => "Teams".into(),
        "FaceTime" => "FaceTime".into(),
        "Webex" => "Webex".into(),
        "Slack" => "Slack".into(),
        other => other.into(),
    }
}

// ── macOS-specific detection ──────────────────────────────────

/// Get list of running process names via `ps`. Fast (~2ms), no permissions
/// needed, no osascript overhead.
fn running_process_names() -> Vec<String> {
    let output = std::process::Command::new("ps")
        .args(["-eo", "comm="])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter_map(|line| {
                    // ps returns full paths like /Applications/zoom.us.app/Contents/MacOS/zoom.us
                    // Extract just the binary name
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        return None;
                    }
                    Some(trimmed.rsplit('/').next().unwrap_or(trimmed).to_string())
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Check if the default audio input device is currently being used.
///
/// Uses a pre-compiled Swift helper that calls CoreAudio
/// `kAudioDevicePropertyDeviceIsRunningSomewhere` on the default input device.
/// Works on both Intel and Apple Silicon Macs.
///
/// Falls back to an inline `swift` invocation if the helper binary is missing.
fn is_mic_in_use() -> bool {
    // Try the pre-compiled helper first (fast: ~5ms)
    let helper = find_mic_check_binary();
    if let Some(path) = &helper {
        if let Ok(out) = std::process::Command::new(path).output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return text == "1";
            }
        }
    }

    // Fallback: inline swift (slower: ~200ms, but always works)
    let script = r#"
import CoreAudio
var id = AudioObjectID(kAudioObjectSystemObject)
var pa = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
var sz = UInt32(MemoryLayout<AudioObjectID>.size)
guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &pa, 0, nil, &sz, &id) == noErr else { print("0"); exit(0) }
var r: UInt32 = 0
var ra = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
sz = UInt32(MemoryLayout<UInt32>.size)
guard AudioObjectGetPropertyData(id, &ra, 0, nil, &sz, &r) == noErr else { print("0"); exit(0) }
print(r > 0 ? "1" : "0")
"#;

    let output = std::process::Command::new("swift")
        .arg("-e")
        .arg(script)
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "1",
        _ => false,
    }
}

/// Find the pre-compiled mic_check binary.
/// Checks next to the app binary first, then the source tree location.
fn find_mic_check_binary() -> Option<std::path::PathBuf> {
    // In the bundled app: same directory as the main binary
    if let Ok(exe) = std::env::current_exe() {
        let beside_exe = exe.parent().unwrap_or(exe.as_ref()).join("mic_check");
        if beside_exe.exists() {
            return Some(beside_exe);
        }
    }

    // In development: check the source tree
    let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin/mic_check");
    if dev_path.exists() {
        return Some(dev_path);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_session_rearms_when_process_changes_or_ends() {
        let detector = CallDetector::new(CallDetectionConfig {
            enabled: true,
            poll_interval_secs: 1,
            cooldown_minutes: 5,
            apps: vec!["zoom.us".into()],
        });

        assert!(matches!(
            detector.note_active_call("zoom.us"),
            DetectionTransition::NewSession
        ));
        assert!(matches!(
            detector.note_active_call("zoom.us"),
            DetectionTransition::Noop
        ));
        detector.clear_active_call();
        assert!(matches!(
            detector.note_active_call("zoom.us"),
            DetectionTransition::NewSession
        ));
        assert!(matches!(
            detector.note_active_call("face.time"),
            DetectionTransition::NewSession
        ));
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name_for("zoom.us"), "Zoom");
        assert_eq!(display_name_for("Microsoft Teams"), "Teams");
        assert_eq!(display_name_for("FaceTime"), "FaceTime");
        assert_eq!(display_name_for("SomeOtherApp"), "SomeOtherApp");
    }

    #[test]
    fn process_list_returns_real_results() {
        let procs = running_process_names();
        // ps should always return at least a few processes
        assert!(!procs.is_empty(), "process list should not be empty");
    }

    #[test]
    fn mic_check_does_not_panic() {
        // Just verify the function returns without crashing.
        // Will return false unless something is using the mic right now.
        let _result = is_mic_in_use();
    }
}
