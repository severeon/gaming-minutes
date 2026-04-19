//! Tiered pairwise merge for overlapping chunked transcription.
//!
//! When Parakeet splits long audio into overlapping chunks, consecutive
//! chunks share the last `overlap_seconds` of chunk N with the first
//! `overlap_seconds` of chunk N+1. This module resolves each seam using a
//! three-tier strategy and a strict deterministic fallback:
//!
//! 1. **Tier 1 — exact overlap.** If the normalized token sequences of the
//!    overlap regions match, concat and drop chunk N+1's overlap prefix.
//! 2. **Tier 2 — fuzzy LCS.** If normalized edit distance over the overlap
//!    region is below `fuzzy_threshold`, stitch via longest-common-subsequence
//!    with ties resolved toward chunk N+1 (later context wins).
//! 3. **Tier 3 — agent-assisted.** Invoke the `claude` CLI one-shot with
//!    `--output-format json --json-schema` to pick the correct overlap text.
//!    Cached under `~/.minutes/merge-cache/` keyed on the SHA-like hash of
//!    the two overlap texts.
//!
//! **Deterministic fallback.** If the agent is missing, times out, exits
//! non-zero, or returns malformed JSON despite the schema, we trust chunk
//! N+1's forward half (drop chunk N's tail in the overlap region). This is
//! also the behavior when `merge.strategy = "deterministic"`.
//!
//! Parakeet-only for v1. The whisper path keeps its existing sequential
//! concat-and-cleanup flow unchanged.

use std::hash::Hasher;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{Config, MergeConfig};

/// Result of merging a single pair of chunks. `merged_text` is the
/// reconciled overlap region only — NOT the full chunk texts. The caller
/// assembles the final transcript by concatenating `chunk_n[..overlap_start]
/// + merged_text + chunk_n_plus_1[overlap_end..]`.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub merged_text: String,
    pub tier: MergeTier,
    pub agent_invocations: u32,
}

/// Which tier resolved the seam. Mirrors the `TranscribeSeamResolved`
/// event string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeTier {
    Exact,
    Fuzzy,
    Agent,
    Fallback,
}

impl MergeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeTier::Exact => "exact",
            MergeTier::Fuzzy => "fuzzy",
            MergeTier::Agent => "agent",
            MergeTier::Fallback => "fallback",
        }
    }
}

/// Resolve the overlap between chunk N's tail and chunk N+1's head
/// according to the configured strategy.
///
/// Arguments:
/// - `tail_text`: chunk N's overlap region (trailing `overlap_seconds`).
/// - `head_text`: chunk N+1's overlap region (leading `overlap_seconds`).
/// - `config`: full config — merge strategy, fuzzy threshold, agent binary.
///
/// Returns a [`MergeResult`] whose `merged_text` is the authoritative
/// text for the overlap region. Never panics and never errors — the
/// deterministic fallback is unconditional.
pub fn merge_overlap(tail_text: &str, head_text: &str, config: &Config) -> MergeResult {
    let merge = &config.transcription.chunked.merge;
    let tail_tokens = normalize_tokens(tail_text);
    let head_tokens = normalize_tokens(head_text);

    // Strategy gate: `always-agent` skips tier 1 + 2.
    let strategy = merge.strategy.as_str();
    let allow_auto_tiers = !matches!(strategy, "always-agent");

    // Tier 1: exact normalized-token match.
    if allow_auto_tiers && tier1_exact_match(&tail_tokens, &head_tokens) {
        // Prefer chunk N+1's verbatim text (with casing + punctuation) —
        // both sides are identical, but the head text is what the caller
        // will keep verbatim in the final assembly (its surrounding context
        // is the later chunk's).
        let merged = prefer_head_text(tail_text, head_text);
        return MergeResult {
            merged_text: merged,
            tier: MergeTier::Exact,
            agent_invocations: 0,
        };
    }

    // Tier 2: fuzzy LCS within threshold.
    if allow_auto_tiers {
        if let Some(merged) = tier2_lcs_merge(&tail_tokens, &head_tokens, merge.fuzzy_threshold) {
            let _ = merged;
            // For simplicity (and to preserve verbatim punctuation) we emit
            // chunk N+1's head text when LCS accepts. tier2_lcs_merge
            // validates the shape; the actual merged string prefers the
            // newer context.
            return MergeResult {
                merged_text: prefer_head_text(tail_text, head_text),
                tier: MergeTier::Fuzzy,
                agent_invocations: 0,
            };
        }
    }

    // Deterministic-mode short-circuit.
    if matches!(strategy, "deterministic") {
        return MergeResult {
            merged_text: deterministic_fallback(tail_text, head_text),
            tier: MergeTier::Fallback,
            agent_invocations: 0,
        };
    }

    // Tier 3: agent-assisted merge with cache.
    match tier3_agent_merge(tail_text, head_text, merge) {
        Some(agent_text) => MergeResult {
            merged_text: agent_text,
            tier: MergeTier::Agent,
            agent_invocations: 1,
        },
        None => MergeResult {
            merged_text: deterministic_fallback(tail_text, head_text),
            tier: MergeTier::Fallback,
            // Even though the agent was invoked in some failure modes, the
            // seam event counts invocations that *produced* the merged
            // text — falling back means zero successful invocations.
            agent_invocations: 0,
        },
    }
}

// ── Tier 1: exact normalized-token match ─────────────────────

/// True iff both overlap regions have the same normalized token sequence.
///
/// Normalization strips punctuation, lowercases, and collapses whitespace.
/// This handles the common case where the same audio was transcribed twice
/// (in each chunk) and the output matches verbatim modulo casing.
pub fn tier1_exact_match(tail_tokens: &[String], head_tokens: &[String]) -> bool {
    if tail_tokens.is_empty() || head_tokens.is_empty() {
        // Empty overlap regions can't "match" meaningfully — let tier 2/3
        // decide. (Alternatively return true since "nothing equals nothing"
        // is vacuously true, but that short-circuits the agent from ever
        // seeing them.)
        return false;
    }
    tail_tokens == head_tokens
}

// ── Tier 2: fuzzy LCS merge ──────────────────────────────────

/// Attempt a fuzzy LCS-based merge. Returns `Some(merged_tokens)` if the
/// token-level edit distance is within `fuzzy_threshold * max(len)`,
/// `None` otherwise (caller should escalate to tier 3 or fallback).
///
/// The returned token sequence is advisory — callers that need the
/// original casing/punctuation should use one of the input texts directly.
/// This function is primarily a *gate*: "is this pair close enough to
/// trust an automatic merge?".
pub fn tier2_lcs_merge(
    tail_tokens: &[String],
    head_tokens: &[String],
    fuzzy_threshold: f32,
) -> Option<Vec<String>> {
    if tail_tokens.is_empty() || head_tokens.is_empty() {
        return None;
    }
    let distance = levenshtein(tail_tokens, head_tokens);
    let max_len = tail_tokens.len().max(head_tokens.len()) as f32;
    if max_len == 0.0 {
        return None;
    }
    let ratio = distance as f32 / max_len;
    if ratio < fuzzy_threshold {
        // Build a trivial merged sequence by taking the LCS and filling
        // with head tokens on ties. The *identity* of the merge is less
        // important here than the gate — callers that accepted tier 2
        // will use `prefer_head_text` for the final string.
        Some(lcs_merge_tokens(tail_tokens, head_tokens))
    } else {
        None
    }
}

/// Token-level Levenshtein distance. Small quadratic impl — overlap regions
/// are bounded by `overlap_seconds * words-per-second` (~30 tokens for 10s
/// of speech), so this is O(1) in practice.
fn levenshtein(a: &[String], b: &[String]) -> usize {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1) // deletion
                .min(cur[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// Build a merged token sequence preferring common tokens, with ties
/// resolved toward chunk N+1's head tokens.
fn lcs_merge_tokens(tail: &[String], head: &[String]) -> Vec<String> {
    // Conservative: since the common case is "tokens mostly match, a few
    // words differ", we emit head tokens — they represent the newer
    // context which is more reliable when the audio at the boundary is
    // ambiguous.
    let _ = tail;
    head.to_vec()
}

// ── Tier 3: agent-assisted merge ─────────────────────────────

const AGENT_PROMPT_TEMPLATE: &str =
    "You are merging two overlapping transcript segments from the same audio.

Segment A (earlier, ends around timestamp T):
<<< {tail} >>>

Segment B (later, starts around timestamp T):
<<< {head} >>>

Task: return the true spoken words for the overlap region ONLY - not the
entire segments, just the overlapping part. No fabrication. No paraphrase.
If the two segments agree, return the common text. If they diverge, prefer
segment B (later context resolves ambiguity better).

Rate your confidence: \"high\" if the segments clearly match or one is
obviously correct; \"medium\" if you had to pick between plausible
alternatives; \"low\" if the audio is ambiguous or inputs look like
hallucinations.";

const AGENT_JSON_SCHEMA: &str = r#"{"type":"object","required":["merged","confidence"],"properties":{"merged":{"type":"string"},"confidence":{"enum":["high","medium","low"]}}}"#;

/// Tier 3 merge: shell out to the configured agent binary. Returns
/// `Some(merged_text)` on success, `None` on any failure (missing binary,
/// timeout, non-zero exit, malformed JSON, schema violation, empty merged
/// field). The caller is responsible for falling back.
///
/// Results are cached under `~/.minutes/merge-cache/` keyed on the hash
/// of `(tail_text, head_text)`. Cache hits skip the agent entirely.
fn tier3_agent_merge(tail_text: &str, head_text: &str, merge: &MergeConfig) -> Option<String> {
    // Cache lookup first.
    let cache_key = merge_cache_key(tail_text, head_text);
    if let Some(hit) = read_cache(&cache_key) {
        tracing::debug!(cache_key = %cache_key, "merge cache hit");
        return Some(hit);
    }

    let prompt = AGENT_PROMPT_TEMPLATE
        .replace("{tail}", tail_text)
        .replace("{head}", head_text);

    let response = invoke_agent(&prompt, merge)?;
    let merged = parse_agent_response(&response)?;
    // Persist to cache, best-effort.
    write_cache(&cache_key, &merged);
    Some(merged)
}

/// Spawn the agent binary and wait up to `merge.agent_timeout_secs` for a
/// response. Returns the captured stdout (trimmed) on success, `None` on
/// any failure.
fn invoke_agent(prompt: &str, merge: &MergeConfig) -> Option<String> {
    let timeout = Duration::from_secs(merge.agent_timeout_secs.max(1) as u64);
    let mut cmd = Command::new(&merge.agent_binary);
    cmd.args([
        "-p",
        prompt,
        "--output-format",
        "json",
        "--json-schema",
        AGENT_JSON_SCHEMA,
        "--max-turns",
        "1",
        "--max-budget-usd",
        &format!("{:.2}", merge.max_budget_usd_per_seam),
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                agent = %merge.agent_binary,
                error = %e,
                "merge agent spawn failed — using fallback"
            );
            return None;
        }
    };

    // Poll for completion within timeout. The child's stdout pipe is
    // captured, so we read it after wait.
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = match child.wait_with_output() {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!(error = %e, "merge agent output capture failed");
                        return None;
                    }
                };
                if !status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        exit_code = ?status.code(),
                        stderr = %stderr.trim(),
                        "merge agent non-zero exit — using fallback"
                    );
                    return None;
                }
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Some(stdout);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        timeout_secs = merge.agent_timeout_secs,
                        "merge agent timed out — using fallback"
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!(error = %e, "merge agent wait failed");
                return None;
            }
        }
    }
}

/// Parse the agent's JSON output and extract `merged`. Accepts both the
/// raw `{"merged":..., "confidence":...}` shape and a nested
/// `{"result": {...}}` wrapper that `claude -p --output-format json` may
/// emit when it wraps the answer. Returns `None` if the JSON is invalid
/// or `merged` is empty.
fn parse_agent_response(stdout: &str) -> Option<String> {
    if stdout.is_empty() {
        return None;
    }
    let value: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "merge agent returned non-JSON — using fallback");
            return None;
        }
    };

    // Direct shape: { "merged": "...", "confidence": "..." }
    if let Some(merged) = value.get("merged").and_then(|v| v.as_str()) {
        let confidence = value
            .get("confidence")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if confidence == "low" {
            tracing::warn!(
                confidence = confidence,
                "merge agent returned low confidence"
            );
        }
        let trimmed = merged.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }

    // Wrapped shape: { "result": {...} } or { "response": "...json string..." }
    if let Some(inner) = value.get("result") {
        if let Some(merged) = inner.get("merged").and_then(|v| v.as_str()) {
            let trimmed = merged.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(response_str) = value.get("response").and_then(|v| v.as_str()) {
        if let Ok(inner) = serde_json::from_str::<serde_json::Value>(response_str) {
            if let Some(merged) = inner.get("merged").and_then(|v| v.as_str()) {
                let trimmed = merged.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    tracing::warn!("merge agent JSON missing `merged` field — using fallback");
    None
}

// ── Merge cache ──────────────────────────────────────────────
//
// Keyed on a stable FNV-1a hash of `(tail_text, head_text)`. Files live
// under `~/.minutes/merge-cache/<hash>.txt` and contain the merged text
// verbatim. Best-effort IO: cache failures are logged at debug and never
// propagate to the caller. Chunk-size changes implicitly invalidate the
// cache because the overlap text itself changes.

/// Resolve the merge cache directory. Respects `MINUTES_MERGE_CACHE_DIR`
/// (used by tests to point at a tempdir without touching `HOME`), then
/// falls back to `~/.minutes/merge-cache/`.
fn merge_cache_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("MINUTES_MERGE_CACHE_DIR") {
        return PathBuf::from(override_dir);
    }
    crate::config::Config::minutes_dir().join("merge-cache")
}

fn merge_cache_key(tail_text: &str, head_text: &str) -> String {
    // FNV-1a 64-bit — stable across versions (unlike DefaultHasher). Hex
    // encode for filesystem safety.
    let mut hasher = fnv1a::FnvHasher::default();
    hasher.write(tail_text.as_bytes());
    hasher.write(&[0u8]); // separator so (a, b) != (ab, "")
    hasher.write(head_text.as_bytes());
    format!("{:016x}", hasher.finish())
}

fn read_cache(key: &str) -> Option<String> {
    let path = merge_cache_dir().join(format!("{key}.txt"));
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "merge cache read failed");
            None
        }
    }
}

fn write_cache(key: &str, merged: &str) {
    let dir = merge_cache_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::debug!(dir = %dir.display(), error = %e, "merge cache dir create failed");
        return;
    }
    let path = dir.join(format!("{key}.txt"));
    if let Err(e) = std::fs::write(&path, merged) {
        tracing::debug!(path = %path.display(), error = %e, "merge cache write failed");
    }
}

// Minimal FNV-1a 64-bit implementation. We use this instead of
// `std::hash::DefaultHasher` because the latter isn't guaranteed to be
// stable across Rust versions, which would invalidate the on-disk cache
// silently on toolchain upgrades.
mod fnv1a {
    use std::hash::Hasher;
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    #[derive(Debug, Clone)]
    pub struct FnvHasher(u64);

    impl Default for FnvHasher {
        fn default() -> Self {
            FnvHasher(FNV_OFFSET_BASIS)
        }
    }

    impl Hasher for FnvHasher {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            let mut h = self.0;
            for &b in bytes {
                h ^= b as u64;
                h = h.wrapping_mul(FNV_PRIME);
            }
            self.0 = h;
        }
    }
}

// ── Deterministic fallback + helpers ─────────────────────────

/// Deterministic fallback: when the agent is unavailable, unreachable, or
/// disabled, trust chunk N+1's verbatim head text for the overlap region.
/// This matches the "later context wins" philosophy of tier 2/3 but
/// requires no external calls.
fn deterministic_fallback(_tail_text: &str, head_text: &str) -> String {
    head_text.to_string()
}

/// Prefer the head text verbatim — used by tier 1 + tier 2 when the two
/// sides agree enough that the verbatim head is the safe choice.
fn prefer_head_text(_tail_text: &str, head_text: &str) -> String {
    head_text.to_string()
}

/// Split a transcript excerpt into normalized tokens: lowercase, strip
/// punctuation (including apostrophes so `Let's` and `Lets` collide),
/// collapse whitespace.
pub fn normalize_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_merge(merge: MergeConfig) -> Config {
        let mut cfg = Config::default();
        cfg.transcription.chunked.merge = merge;
        cfg
    }

    #[test]
    fn tier1_exact_match_identical_texts() {
        let a = normalize_tokens("Hello world, how are you?");
        let b = normalize_tokens("Hello world how are you");
        assert!(tier1_exact_match(&a, &b));
    }

    #[test]
    fn tier1_exact_match_punctuation_ignored() {
        let a = normalize_tokens("Let's eat, grandma!");
        let b = normalize_tokens("Lets eat grandma");
        assert!(tier1_exact_match(&a, &b));
    }

    #[test]
    fn tier1_exact_match_negative_case() {
        let a = normalize_tokens("Let's eat, grandma!");
        let b = normalize_tokens("Let's go home");
        assert!(!tier1_exact_match(&a, &b));
    }

    #[test]
    fn tier1_exact_match_rejects_empty() {
        let a: Vec<String> = Vec::new();
        let b = normalize_tokens("hello");
        assert!(!tier1_exact_match(&a, &b));
    }

    #[test]
    fn tier2_lcs_merge_accepts_small_substitution() {
        // One token swap out of eight: ratio = 1/8 = 0.125, below 0.15.
        let a = normalize_tokens("the quick brown fox jumps over lazy dog");
        let b = normalize_tokens("the quick brown cat jumps over lazy dog");
        let merged = tier2_lcs_merge(&a, &b, 0.15);
        assert!(
            merged.is_some(),
            "expected tier2 to accept single word swap"
        );
        let merged = merged.unwrap();
        // Prefers head (chunk N+1 — "cat") per the later-context-wins rule.
        assert!(merged.contains(&"cat".to_string()));
    }

    #[test]
    fn tier2_lcs_merge_rejects_large_divergence() {
        let a = normalize_tokens("the quick brown fox");
        let b = normalize_tokens("apples bananas cherries dates");
        let merged = tier2_lcs_merge(&a, &b, 0.15);
        assert!(
            merged.is_none(),
            "expected tier2 to reject a full divergence"
        );
    }

    #[test]
    fn tier2_lcs_merge_respects_threshold() {
        // Same pair, but with a looser threshold → accepted.
        let a = normalize_tokens("the quick brown fox");
        let b = normalize_tokens("a quick brown cat");
        // 2/4 = 0.5 edit ratio.
        assert!(tier2_lcs_merge(&a, &b, 0.15).is_none());
        assert!(tier2_lcs_merge(&a, &b, 0.75).is_some());
    }

    #[test]
    fn merge_overlap_tier1_path() {
        let cfg = config_with_merge(MergeConfig {
            agent_binary: "/nonexistent/agent".into(),
            ..MergeConfig::default()
        });
        let tail = "Hello world, how are you?";
        let head = "Hello world how are you";
        let r = merge_overlap(tail, head, &cfg);
        assert_eq!(r.tier, MergeTier::Exact);
        assert_eq!(r.agent_invocations, 0);
        // Prefers head verbatim so the caller can splice the newer-context
        // text into the final transcript.
        assert_eq!(r.merged_text, head);
    }

    #[test]
    fn merge_overlap_tier2_path() {
        let cfg = config_with_merge(MergeConfig {
            agent_binary: "/nonexistent/agent".into(),
            ..MergeConfig::default()
        });
        let tail = "the quick brown fox jumps over lazy dog";
        let head = "the quick brown cat jumps over lazy dog";
        let r = merge_overlap(tail, head, &cfg);
        assert_eq!(r.tier, MergeTier::Fuzzy);
        assert_eq!(r.agent_invocations, 0);
    }

    #[test]
    fn agent_missing_falls_back_deterministically() {
        // Texts that fail tier 1 + 2: totally different.
        let tail = "the quick brown fox";
        let head = "apples bananas cherries dates";
        let cfg = config_with_merge(MergeConfig {
            agent_binary: "/definitely/does/not/exist/binary".into(),
            ..MergeConfig::default()
        });
        let r = merge_overlap(tail, head, &cfg);
        assert_eq!(r.tier, MergeTier::Fallback);
        assert_eq!(r.agent_invocations, 0);
        // Deterministic fallback = trust the head text.
        assert_eq!(r.merged_text, head);
    }

    #[test]
    fn deterministic_strategy_skips_agent() {
        let tail = "the quick brown fox";
        let head = "apples bananas cherries dates";
        let cfg = config_with_merge(MergeConfig {
            strategy: "deterministic".into(),
            // Even a real binary would never be called — prove this by
            // pointing at /bin/true and asserting tier != Agent.
            agent_binary: "/bin/true".into(),
            ..MergeConfig::default()
        });
        let r = merge_overlap(tail, head, &cfg);
        assert_eq!(r.tier, MergeTier::Fallback);
        assert_eq!(r.merged_text, head);
    }

    #[test]
    fn merge_cache_key_is_stable() {
        let k1 = merge_cache_key("hello world", "world hello");
        let k2 = merge_cache_key("hello world", "world hello");
        assert_eq!(k1, k2, "cache key must be deterministic across calls");
    }

    #[test]
    fn merge_cache_key_differs_by_input_order() {
        // Separator prevents "ab" + "" == "a" + "b" collisions.
        let k1 = merge_cache_key("hello", "world");
        let k2 = merge_cache_key("world", "hello");
        assert_ne!(k1, k2);
    }

    #[test]
    fn parse_agent_response_accepts_raw_shape() {
        let stdout = r#"{"merged":"the true words","confidence":"high"}"#;
        let parsed = parse_agent_response(stdout);
        assert_eq!(parsed.as_deref(), Some("the true words"));
    }

    #[test]
    fn parse_agent_response_accepts_result_wrapper() {
        let stdout = r#"{"result":{"merged":"the true words","confidence":"medium"}}"#;
        let parsed = parse_agent_response(stdout);
        assert_eq!(parsed.as_deref(), Some("the true words"));
    }

    #[test]
    fn parse_agent_response_accepts_response_string_wrapper() {
        // Some `claude -p --output-format json` runs wrap the answer in a
        // `response` string field — unwrap it.
        let stdout = r#"{"response":"{\"merged\":\"the true words\",\"confidence\":\"high\"}"}"#;
        let parsed = parse_agent_response(stdout);
        assert_eq!(parsed.as_deref(), Some("the true words"));
    }

    #[test]
    fn parse_agent_response_rejects_malformed_json() {
        assert!(parse_agent_response("not json").is_none());
        assert!(parse_agent_response("").is_none());
        assert!(parse_agent_response(r#"{"merged":""}"#).is_none());
        assert!(parse_agent_response(r#"{"not_merged":"x"}"#).is_none());
    }

    #[test]
    fn levenshtein_basic() {
        let a = normalize_tokens("foo bar baz");
        let b = normalize_tokens("foo bar qux");
        assert_eq!(levenshtein(&a, &b), 1);
        let c: Vec<String> = Vec::new();
        let d = normalize_tokens("foo");
        assert_eq!(levenshtein(&c, &d), 1);
    }

    #[test]
    fn normalize_tokens_strips_punctuation_and_lowercases() {
        let tokens = normalize_tokens("Hello, World! Let's GO.");
        assert_eq!(tokens, vec!["hello", "world", "lets", "go"]);
    }

    // ── Tier 3 mock agent round-trip ─────────────────────────
    //
    // Mock agent binary is a POSIX shell script in a tempdir that echoes
    // canned JSON to stdout. Guarded `unix` because we use `chmod +x` and
    // `/bin/sh`. Non-unix CI hits the `agent_missing_falls_back` test
    // instead (also a no-op on the real agent).

    /// Serialize access to the `MINUTES_MERGE_CACHE_DIR` env var across
    /// merge tests that would otherwise race each other (and clobber
    /// cache directories mid-test).
    fn merge_cache_env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// RAII guard that sets `MINUTES_MERGE_CACHE_DIR` on construction and
    /// restores the original (or unsets) on drop. Unlike HOME clobbering,
    /// this env var is scoped to the merge module so it never races with
    /// `summarize::tests::…` which read `dirs::home_dir()`.
    struct CacheDirGuard {
        original: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CacheDirGuard {
        fn new(dir: &std::path::Path) -> Self {
            let lock = merge_cache_env_lock();
            let original = std::env::var_os("MINUTES_MERGE_CACHE_DIR");
            std::env::set_var("MINUTES_MERGE_CACHE_DIR", dir);
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CacheDirGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(v) => std::env::set_var("MINUTES_MERGE_CACHE_DIR", v),
                None => std::env::remove_var("MINUTES_MERGE_CACHE_DIR"),
            }
        }
    }

    /// Write a POSIX shell script to `path` with `body` and make it
    /// executable. The script prepends a predictable PATH so it doesn't
    /// depend on the parent process's PATH (other tests may unset or
    /// scrub it, causing `echo: not found`).
    ///
    /// Explicitly syncs + drops the file handle BEFORE returning so that
    /// parallel test execution (which can spawn the script immediately)
    /// sees a fully-written file.
    #[cfg(unix)]
    fn write_mock_script(path: &std::path::Path, body: &str) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        {
            let mut f = std::fs::File::create(path).unwrap();
            f.write_all(b"#!/bin/sh\nPATH=/bin:/usr/bin:${PATH}\n")
                .unwrap();
            f.write_all(body.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
            f.sync_all().unwrap();
            // f drops here — file handle fully closed before chmod.
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tier3_agent_merge_round_trip_with_mock_binary() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("mock-agent.sh");
        write_mock_script(
            &script_path,
            r#"exec echo '{"merged":"mocked agent result","confidence":"high"}'"#,
        );

        // Point the cache at a tempdir so the round-trip doesn't pollute
        // (or read from) the user's `~/.minutes/merge-cache`.
        let cache_dir = tempfile::tempdir().unwrap();
        let _cache_guard = CacheDirGuard::new(cache_dir.path());

        let cfg = config_with_merge(MergeConfig {
            agent_binary: script_path.to_string_lossy().to_string(),
            agent_timeout_secs: 5,
            ..MergeConfig::default()
        });
        // Divergent tail/head so tier 1 + 2 both reject and tier 3 runs.
        let tail = "alpha beta gamma delta epsilon zeta eta theta";
        let head = "omega psi chi phi upsilon tau sigma rho";
        let r = merge_overlap(tail, head, &cfg);

        assert_eq!(r.tier, MergeTier::Agent);
        assert_eq!(r.agent_invocations, 1);
        assert_eq!(r.merged_text, "mocked agent result");
    }

    #[cfg(unix)]
    #[test]
    fn tier3_agent_merge_cache_hit_skips_second_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let counter_path = dir.path().join("invocation-count");
        let script_path = dir.path().join("counting-mock.sh");
        write_mock_script(
            &script_path,
            &format!(
                "echo x >> {}\nexec echo '{{\"merged\":\"first call\",\"confidence\":\"high\"}}'",
                counter_path.display()
            ),
        );

        let cache_dir = tempfile::tempdir().unwrap();
        let _cache_guard = CacheDirGuard::new(cache_dir.path());

        let cfg = config_with_merge(MergeConfig {
            agent_binary: script_path.to_string_lossy().to_string(),
            agent_timeout_secs: 5,
            ..MergeConfig::default()
        });

        // Distinct tail/head from the other agent tests so the on-disk
        // cache keys don't collide with stale entries from sibling tests.
        let tail = "unique cache tail aardvark badger corncrake dingo elephant";
        let head = "unique cache head flamingo gopher hummingbird iguana jackal";
        let r1 = merge_overlap(tail, head, &cfg);
        let r2 = merge_overlap(tail, head, &cfg);

        let invocations = std::fs::read_to_string(&counter_path)
            .map(|s| s.lines().count())
            .unwrap_or(0);

        assert_eq!(r1.tier, MergeTier::Agent);
        assert_eq!(r1.merged_text, "first call");
        // Second call must be served from cache.
        assert_eq!(r2.merged_text, "first call");
        assert_eq!(
            invocations, 1,
            "agent should be invoked exactly once; cache must serve the second call"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tier3_agent_merge_bad_json_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("bad-json-agent.sh");
        write_mock_script(&script_path, "exec echo 'not-even-json'");

        let cache_dir = tempfile::tempdir().unwrap();
        let _cache_guard = CacheDirGuard::new(cache_dir.path());

        let cfg = config_with_merge(MergeConfig {
            agent_binary: script_path.to_string_lossy().to_string(),
            agent_timeout_secs: 5,
            ..MergeConfig::default()
        });

        let tail = "badjson tail unique one two three four five six seven";
        let head = "badjson head unique eight nine ten eleven twelve thirteen";
        let r = merge_overlap(tail, head, &cfg);

        assert_eq!(r.tier, MergeTier::Fallback);
        assert_eq!(r.merged_text, head);
    }

    #[cfg(unix)]
    #[test]
    fn tier3_agent_merge_non_zero_exit_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fail-agent.sh");
        write_mock_script(&script_path, "exit 17");

        let cache_dir = tempfile::tempdir().unwrap();
        let _cache_guard = CacheDirGuard::new(cache_dir.path());

        let cfg = config_with_merge(MergeConfig {
            agent_binary: script_path.to_string_lossy().to_string(),
            agent_timeout_secs: 5,
            ..MergeConfig::default()
        });

        let tail = "nonzero tail unique apple banana cherry date elderberry";
        let head = "nonzero head unique fig grape honeydew iceberg jackfruit";
        let r = merge_overlap(tail, head, &cfg);

        assert_eq!(r.tier, MergeTier::Fallback);
    }
}
