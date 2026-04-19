//! Tabletop RPG / game session support (`ContentType::Game`).
//!
//! Three responsibilities live here:
//!
//! 1. **Campaign files** — per-campaign TOML at `~/.minutes/campaigns/<slug>.toml`
//!    with `name`, `system`, `party`, `npcs`, `places`, `mechanics_extra`.
//!    Load/save/list helpers feed the `DecodeHints::for_game` constructor.
//! 2. **Canonical lexicon** — a system-keyed set of dice terms, stat names,
//!    common spells/conditions that primes the transcriber's phrase boost
//!    list so "DC 15" doesn't become "decent 15" and "nat 20" stays "nat 20".
//! 3. **Banter-fold classifier** — post-transcription agent call that returns
//!    `segment_spans` marking which lines are in-game vs. banter vs. mechanics
//!    vs. side-conversation. Advisory metadata only — the full transcript is
//!    always preserved verbatim. The fold-to-`<details>` rendering lives in
//!    `crate::markdown::fold_non_game_spans`.
//!
//! The classifier invocation is a one-shot `claude -p` call with
//! `--output-format json --json-schema <schema> --max-turns 1
//! --max-budget-usd <cap>`. If the binary is missing, times out, or returns
//! a payload that fails validation twice, the pipeline degrades gracefully:
//! the transcript is written without `segment_spans`, the rendering stays
//! flat, and a warning is logged.

use crate::config::Config;
use crate::markdown::{SegmentSpan, SpanClass};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Metadata for a single campaign. Serialized to TOML on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameCampaign {
    /// Human-readable campaign name (e.g. "The Shattered Isles").
    pub name: String,
    /// RPG system identifier (e.g. "5e", "pf2e", "cocv7"). Free-form;
    /// recognized presets inform the canonical lexicon, unknown values
    /// fall back to the generic lexicon.
    #[serde(default = "default_system")]
    pub system: String,
    /// Player character names.
    #[serde(default)]
    pub party: Vec<String>,
    /// Non-player character names.
    #[serde(default)]
    pub npcs: Vec<String>,
    /// Locations, regions, planes, and notable props.
    #[serde(default)]
    pub places: Vec<String>,
    /// Campaign-specific mechanics vocabulary beyond the canonical lexicon
    /// (homebrew features, subclass names, houseruled conditions, etc.).
    #[serde(default)]
    pub mechanics_extra: Vec<String>,
}

fn default_system() -> String {
    "5e".into()
}

impl GameCampaign {
    /// Fresh campaign skeleton for `minutes games init`.
    pub fn new(name: &str, system: &str) -> Self {
        Self {
            name: name.to_string(),
            system: system.to_string(),
            party: Vec::new(),
            npcs: Vec::new(),
            places: Vec::new(),
            mechanics_extra: Vec::new(),
        }
    }

    /// Names the user cares about most — party and NPCs. These fill the
    /// `priority_phrases` slot of `DecodeHints`, which gets the strongest
    /// transcriber boost (bounded at 8).
    pub fn priority_names(&self) -> Vec<String> {
        let mut names = Vec::with_capacity(self.party.len() + self.npcs.len());
        for name in &self.party {
            names.push(name.clone());
        }
        for name in &self.npcs {
            names.push(name.clone());
        }
        names
    }

    /// Terms that round out context — places and bespoke mechanics.
    pub fn contextual_terms(&self) -> Vec<String> {
        let mut terms = Vec::with_capacity(self.places.len() + self.mechanics_extra.len());
        for place in &self.places {
            terms.push(place.clone());
        }
        for mech in &self.mechanics_extra {
            terms.push(mech.clone());
        }
        terms
    }
}

/// Turn a campaign name into a filesystem-safe slug for
/// `<slug>.toml` / CLI `--campaign <slug>`.
pub fn slugify(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    let mut last_dash = true;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "campaign".into()
    } else {
        slug
    }
}

/// Resolve `<campaigns_dir>/<slug>.toml`.
pub fn campaign_path(config: &Config, slug: &str) -> PathBuf {
    config.games.campaigns_dir.join(format!("{}.toml", slug))
}

/// Save a campaign to disk. Creates the campaigns directory if needed.
/// Writes atomically via tmp-and-rename to avoid partial files on crash.
pub fn save_campaign(
    config: &Config,
    slug: &str,
    campaign: &GameCampaign,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(&config.games.campaigns_dir)?;
    let path = campaign_path(config, slug);
    let toml = toml::to_string_pretty(campaign)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Load a campaign by slug. Returns `Ok(None)` if the file does not exist.
pub fn load_campaign(config: &Config, slug: &str) -> std::io::Result<Option<GameCampaign>> {
    let path = campaign_path(config, slug);
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)?;
    let campaign: GameCampaign = toml::from_str(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some(campaign))
}

/// Summary of a campaign for `list_games` / `minutes games list`. Counts
/// sessions by finding markdown files under `<output_dir>/games/` whose
/// `tags` / `source` reference this campaign slug (or — fallback for older
/// data — whose filename contains the slug).
#[derive(Debug, Clone, Serialize)]
pub struct GameCampaignSummary {
    pub slug: String,
    pub name: String,
    pub system: String,
    pub session_count: usize,
}

/// List all campaigns in `config.games.campaigns_dir`. Silently skips files
/// that don't parse; malformed TOML is ignored rather than fatal.
pub fn list_campaigns(config: &Config) -> std::io::Result<Vec<GameCampaignSummary>> {
    let mut out = Vec::new();
    let dir = &config.games.campaigns_dir;
    if !dir.exists() {
        return Ok(out);
    }
    let games_dir = config.output_dir.join("games");
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let slug = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let campaign: GameCampaign = match toml::from_str(&body) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let session_count = count_sessions_for_slug(&games_dir, &slug);
        out.push(GameCampaignSummary {
            slug,
            name: campaign.name,
            system: campaign.system,
            session_count,
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Count sessions that reference this campaign slug. Cheap heuristic: scan
/// filenames for the slug (since meeting writes embed slugified titles).
/// Full-fidelity attribution via frontmatter would require parsing every
/// file — we keep this fast by design.
fn count_sessions_for_slug(games_dir: &Path, slug: &str) -> usize {
    if !games_dir.exists() {
        return 0;
    }
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(games_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.contains(slug) {
                count += 1;
            }
        }
    }
    count
}

// ────────────────────────────────────────────────────────────
// Canonical RPG lexicon
// ────────────────────────────────────────────────────────────

/// Return the canonical phrase-boost lexicon for the given system. Lowercase
/// systems recognized: "5e" / "dnd" / "dungeons and dragons" (D&D 5th ed),
/// "pf2e" / "pf" (Pathfinder 2e), "cocv7" / "coc" (Call of Cthulhu). Anything
/// else returns a generic dice+action lexicon that works across most d20
/// systems.
pub fn lexicon_for_system(system: &str) -> Vec<String> {
    let key = system.trim().to_lowercase();
    match key.as_str() {
        "5e" | "dnd" | "dungeons and dragons" | "d&d" | "dungeons & dragons" => dnd_5e_lexicon(),
        "pf2e" | "pf" | "pathfinder" | "pathfinder 2e" => pathfinder_lexicon(),
        "cocv7" | "coc" | "call of cthulhu" => call_of_cthulhu_lexicon(),
        _ => generic_lexicon(),
    }
}

fn generic_lexicon() -> Vec<String> {
    [
        "d4",
        "d6",
        "d8",
        "d10",
        "d12",
        "d20",
        "d100",
        "nat 20",
        "nat 1",
        "critical hit",
        "critical failure",
        "saving throw",
        "attack roll",
        "damage roll",
        "initiative",
        "advantage",
        "disadvantage",
        "modifier",
        "skill check",
        "perception check",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn dnd_5e_lexicon() -> Vec<String> {
    let mut terms: Vec<&str> = vec![
        // Dice
        "d4",
        "d6",
        "d8",
        "d10",
        "d12",
        "d20",
        "d100",
        "nat 20",
        "nat 1",
        "critical hit",
        "critical failure",
        "advantage",
        "disadvantage",
        // Core mechanics
        "saving throw",
        "attack roll",
        "damage roll",
        "initiative",
        "armor class",
        "hit points",
        "hit dice",
        "proficiency bonus",
        "ability check",
        "skill check",
        "passive perception",
        "death save",
        // Ability scores
        "Strength",
        "Dexterity",
        "Constitution",
        "Intelligence",
        "Wisdom",
        "Charisma",
        // Skills
        "Athletics",
        "Acrobatics",
        "Stealth",
        "Perception",
        "Insight",
        "Investigation",
        "Arcana",
        "History",
        "Religion",
        "Nature",
        "Survival",
        "Medicine",
        "Persuasion",
        "Deception",
        "Intimidation",
        "Performance",
        "Sleight of Hand",
        "Animal Handling",
        // Classes
        "Barbarian",
        "Bard",
        "Cleric",
        "Druid",
        "Fighter",
        "Monk",
        "Paladin",
        "Ranger",
        "Rogue",
        "Sorcerer",
        "Warlock",
        "Wizard",
        "Artificer",
        // Conditions
        "prone",
        "grappled",
        "restrained",
        "stunned",
        "paralyzed",
        "frightened",
        "charmed",
        "incapacitated",
        "unconscious",
        "poisoned",
        "petrified",
        "exhausted",
        "concentration",
        // Spells
        "Fireball",
        "Magic Missile",
        "Cure Wounds",
        "Healing Word",
        "Eldritch Blast",
        "Counterspell",
        "Shield",
        "Misty Step",
        "Hold Person",
        "Dispel Magic",
        "Detect Magic",
        "Mage Hand",
        "Prestidigitation",
        "Guidance",
        "Bless",
        "Bardic Inspiration",
        "Hex",
        "Hunter's Mark",
        // Creatures
        "goblin",
        "orc",
        "kobold",
        "dragon",
        "lich",
        "beholder",
        "mind flayer",
        "tarrasque",
        "owlbear",
        "displacer beast",
        // Currency
        "copper",
        "silver",
        "electrum",
        "gold",
        "platinum",
    ];
    terms.sort();
    terms.dedup();
    terms.iter().map(|s| s.to_string()).collect()
}

fn pathfinder_lexicon() -> Vec<String> {
    let mut terms: Vec<&str> = vec![
        "d4",
        "d6",
        "d8",
        "d10",
        "d12",
        "d20",
        "d100",
        "critical success",
        "critical failure",
        "natural 20",
        "natural 1",
        "saving throw",
        "attack roll",
        "damage roll",
        "initiative",
        "armor class",
        "hit points",
        "proficiency",
        "ability check",
        "skill check",
        "Strength",
        "Dexterity",
        "Constitution",
        "Intelligence",
        "Wisdom",
        "Charisma",
        "Athletics",
        "Acrobatics",
        "Stealth",
        "Perception",
        "Society",
        "Arcana",
        "Crafting",
        "Medicine",
        "Nature",
        "Occultism",
        "Religion",
        "Survival",
        "Thievery",
        "Diplomacy",
        "Deception",
        "Intimidation",
        "Performance",
        "Alchemist",
        "Barbarian",
        "Bard",
        "Champion",
        "Cleric",
        "Druid",
        "Fighter",
        "Investigator",
        "Magus",
        "Monk",
        "Oracle",
        "Psychic",
        "Ranger",
        "Rogue",
        "Sorcerer",
        "Summoner",
        "Swashbuckler",
        "Thaumaturge",
        "Witch",
        "Wizard",
        "focus points",
        "focus spell",
        "action economy",
        "reaction",
        "free action",
        "hero point",
        "degrees of success",
    ];
    terms.sort();
    terms.dedup();
    terms.iter().map(|s| s.to_string()).collect()
}

fn call_of_cthulhu_lexicon() -> Vec<String> {
    let mut terms: Vec<&str> = vec![
        "d4",
        "d6",
        "d8",
        "d10",
        "d20",
        "d100",
        "percentile",
        "sanity check",
        "sanity loss",
        "Cthulhu Mythos",
        "Mythos roll",
        "Luck",
        "Credit Rating",
        "push the roll",
        "bonus die",
        "penalty die",
        "Dodge",
        "Fighting",
        "Firearms",
        "Spot Hidden",
        "Listen",
        "Library Use",
        "Occult",
        "Psychology",
        "Stealth",
        "Keeper",
        "investigator",
        "major wound",
        "dying",
        "unconscious",
        "spell",
        "deep one",
        "shoggoth",
        "Nyarlathotep",
        "Azathoth",
        "Yog-Sothoth",
    ];
    terms.sort();
    terms.dedup();
    terms.iter().map(|s| s.to_string()).collect()
}

// ────────────────────────────────────────────────────────────
// Banter-fold classifier
// ────────────────────────────────────────────────────────────

/// Outcome of a classifier invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierOutcome {
    /// Spans validated, write them to frontmatter.
    Ok(Vec<SegmentSpan>),
    /// Agent binary missing, timed out, or exited non-zero.
    Skipped(String),
    /// Output parsed but failed validation twice in a row.
    Rejected(String),
}

/// Strict JSON Schema enforced on the classifier's output by `claude -p
/// --json-schema`. Every line must be covered exactly once, line ranges are
/// 1-based inclusive, and `class` is constrained to the six SpanClass
/// variants.
pub const CLASSIFIER_JSON_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["spans"],
  "properties": {
    "spans": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["first_line", "last_line", "class", "reason"],
        "properties": {
          "first_line": { "type": "integer", "minimum": 1 },
          "last_line":  { "type": "integer", "minimum": 1 },
          "class":      { "type": "string", "enum": ["in_game", "banter", "mechanical", "side_conversation", "break", "unknown"] },
          "reason":     { "type": "string" }
        }
      }
    }
  }
}"#;

fn classifier_prompt(transcript: &str, total_lines: usize) -> String {
    format!(
        "You're reading a tabletop RPG session transcript. The players are playing \
characters in a game world, but they also pause to order food, check phones, discuss \
real life, and handle game mechanics (dice rolls, rule lookups). Classify every line \
into one of: `in_game` (in-character dialogue, description, DM narration), `banter` \
(out-of-character social chat), `mechanical` (dice rolls, rule discussions, character \
sheet bookkeeping), `side_conversation` (pizza, breaks, interruptions), `break` \
(explicit pauses), `unknown` (unclear). Return every line exactly once; spans may be \
single-line. `reason` is a short phrase the user can read at a glance. \
The transcript has {total_lines} numbered lines (1-based). Every integer 1..{total_lines} \
must be covered by exactly one span (no gaps, no overlaps) and `first_line <= last_line`.\n\n\
TRANSCRIPT:\n{transcript}"
    )
}

fn classifier_strict_retry_prompt(transcript: &str, total_lines: usize, reason: &str) -> String {
    format!(
        "Your previous response was rejected because: {reason}. Retry. Return JSON matching \
the schema EXACTLY. Every line number from 1 to {total_lines} must appear in exactly one \
span. No gaps, no overlaps, no numbers outside 1..{total_lines}. `first_line <= last_line`.\n\n\
TRANSCRIPT:\n{transcript}"
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClassifierResponse {
    spans: Vec<ClassifierResponseSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClassifierResponseSpan {
    first_line: usize,
    last_line: usize,
    class: String,
    #[serde(default)]
    reason: String,
}

impl ClassifierResponseSpan {
    fn into_span(self) -> Option<SegmentSpan> {
        let class = match self.class.as_str() {
            "in_game" => SpanClass::InGame,
            "banter" => SpanClass::Banter,
            "mechanical" => SpanClass::Mechanical,
            "side_conversation" => SpanClass::SideConversation,
            "break" => SpanClass::Break,
            "unknown" => SpanClass::Unknown,
            _ => return None,
        };
        Some(SegmentSpan {
            first_line: self.first_line,
            last_line: self.last_line,
            class,
            reason: self.reason,
        })
    }
}

/// Validate a classifier response:
/// - Every line in `1..=total_lines` is covered exactly once.
/// - `first_line <= last_line` for every span.
/// - All span bounds are in-range.
///
/// On failure returns a short phrase explaining the first problem found.
pub fn validate_spans(spans: &[SegmentSpan], total_lines: usize) -> Result<(), String> {
    if total_lines == 0 {
        if !spans.is_empty() {
            return Err("expected 0 spans for empty transcript".into());
        }
        return Ok(());
    }
    if spans.is_empty() {
        return Err("no spans returned".into());
    }

    let mut covered = vec![false; total_lines];
    let mut sorted: Vec<&SegmentSpan> = spans.iter().collect();
    sorted.sort_by_key(|s| s.first_line);

    let mut expected = 1usize;
    for span in &sorted {
        if span.first_line == 0 || span.last_line == 0 {
            return Err("span line numbers must be 1-based".into());
        }
        if span.first_line > span.last_line {
            return Err(format!(
                "span first_line={} > last_line={}",
                span.first_line, span.last_line
            ));
        }
        if span.last_line > total_lines {
            return Err(format!(
                "span last_line={} out of range (transcript has {} lines)",
                span.last_line, total_lines
            ));
        }
        if span.first_line != expected {
            if span.first_line < expected {
                return Err(format!(
                    "span overlaps previous coverage at line {}",
                    span.first_line
                ));
            }
            return Err(format!(
                "gap between line {} and span starting at {}",
                expected - 1,
                span.first_line
            ));
        }
        for line in span.first_line..=span.last_line {
            let idx = line - 1;
            if covered[idx] {
                return Err(format!("line {} covered more than once", line));
            }
            covered[idx] = true;
        }
        expected = span.last_line + 1;
    }

    if expected <= total_lines {
        return Err(format!(
            "trailing gap from line {} to {}",
            expected, total_lines
        ));
    }
    Ok(())
}

/// Invoke the classifier agent binary over the transcript and return span
/// results. Retries once with a stricter prompt on validation failure. Any
/// other failure (missing binary, non-zero exit, timeout) returns
/// `Skipped(...)` — the pipeline should proceed without spans.
pub fn classify_transcript(config: &Config, transcript: &str) -> ClassifierOutcome {
    let classifier = &config.games.classifier;
    if !classifier.enabled {
        return ClassifierOutcome::Skipped("classifier disabled in config".into());
    }

    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        return ClassifierOutcome::Skipped("empty transcript".into());
    }

    let total_lines = trimmed.lines().count();
    let numbered = number_transcript_for_prompt(trimmed);

    let prompt = classifier_prompt(&numbered, total_lines);
    match run_classifier_agent(classifier, &prompt) {
        Ok(raw) => match parse_and_validate(&raw, total_lines) {
            Ok(spans) => ClassifierOutcome::Ok(spans),
            Err(first_err) => {
                tracing::warn!(error = %first_err, "classifier validation failed — retrying");
                let retry_prompt =
                    classifier_strict_retry_prompt(&numbered, total_lines, &first_err);
                match run_classifier_agent(classifier, &retry_prompt) {
                    Ok(raw2) => match parse_and_validate(&raw2, total_lines) {
                        Ok(spans) => ClassifierOutcome::Ok(spans),
                        Err(second_err) => ClassifierOutcome::Rejected(format!(
                            "validation failed twice: {}; {}",
                            first_err, second_err
                        )),
                    },
                    Err(e) => ClassifierOutcome::Skipped(format!("retry failed: {}", e)),
                }
            }
        },
        Err(e) => ClassifierOutcome::Skipped(e),
    }
}

fn number_transcript_for_prompt(transcript: &str) -> String {
    let mut out = String::with_capacity(transcript.len() + 64);
    for (i, line) in transcript.lines().enumerate() {
        use std::fmt::Write as FmtWrite;
        let _ = writeln!(out, "{}: {}", i + 1, line);
    }
    out
}

fn parse_and_validate(raw: &str, total_lines: usize) -> Result<Vec<SegmentSpan>, String> {
    let trimmed = raw.trim();
    // claude -p --output-format json wraps the response in an envelope with
    // a `result` string — if we see that, extract and parse it. Otherwise
    // try to parse the raw output as the classifier payload directly.
    let payload: ClassifierResponse = parse_classifier_payload(trimmed)?;
    let mut spans: Vec<SegmentSpan> = Vec::with_capacity(payload.spans.len());
    for raw_span in payload.spans {
        let span = raw_span
            .clone()
            .into_span()
            .ok_or_else(|| format!("unknown span class: {}", raw_span.class))?;
        spans.push(span);
    }
    validate_spans(&spans, total_lines)?;
    Ok(spans)
}

fn parse_classifier_payload(text: &str) -> Result<ClassifierResponse, String> {
    // First try: text IS the payload.
    if let Ok(payload) = serde_json::from_str::<ClassifierResponse>(text) {
        return Ok(payload);
    }
    // Fallback: claude envelope with a `result` string holding the payload.
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        spans: Option<Vec<ClassifierResponseSpan>>,
    }
    if let Ok(envelope) = serde_json::from_str::<Envelope>(text) {
        if let Some(spans) = envelope.spans {
            return Ok(ClassifierResponse { spans });
        }
        if let Some(result) = envelope.result {
            if let Ok(payload) = serde_json::from_str::<ClassifierResponse>(&result) {
                return Ok(payload);
            }
        }
    }
    Err("could not parse classifier payload as JSON".into())
}

fn run_classifier_agent(
    classifier: &crate::config::GamesClassifierConfig,
    prompt: &str,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(classifier.agent_timeout_secs);
    let mut command = Command::new(&classifier.agent_binary);
    command
        .args([
            "-p",
            prompt,
            "--output-format",
            "json",
            "--json-schema",
            CLASSIFIER_JSON_SCHEMA,
            "--max-turns",
            "1",
            "--max-budget-usd",
            &format!("{}", classifier.max_budget_usd),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "classifier agent binary '{}' not found on PATH",
                classifier.agent_binary
            )
        } else {
            format!("failed to spawn classifier: {}", e)
        }
    })?;

    // Best-effort timeout loop: wait_timeout isn't in std, so poll with
    // try_wait. The prompt is already on stdin-null and fed via args, so we
    // only need to drain stdout/stderr at the end.
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "classifier timed out after {}s",
                        classifier.agent_timeout_secs
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                return Err(format!("failed to poll classifier: {}", e));
            }
        }
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "classifier exited with status {}: {}",
            output.status,
            stderr.lines().last().unwrap_or("unknown error")
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Write a prompt-and-schema pair to a debug file. Used by tests and by the
/// `minutes games process --debug` path to capture what was actually sent.
/// Not used in the normal path.
#[doc(hidden)]
pub fn write_classifier_debug(dir: &Path, prompt: &str, schema: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::File::create(dir.join("classifier-prompt.txt"))?;
    f.write_all(prompt.as_bytes())?;
    let mut g = std::fs::File::create(dir.join("classifier-schema.json"))?;
    g.write_all(schema.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config_with_campaigns(dir: &Path) -> Config {
        Config {
            games: crate::config::GamesConfig {
                campaigns_dir: dir.to_path_buf(),
                default_system: "5e".into(),
                classifier: crate::config::GamesClassifierConfig::default(),
            },
            ..Config::default()
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("The Shattered Isles"), "the-shattered-isles");
        assert_eq!(slugify("Campaign #3!!"), "campaign-3");
        assert_eq!(slugify("   "), "campaign");
    }

    #[test]
    fn campaign_roundtrip() {
        let dir = TempDir::new().unwrap();
        let config = config_with_campaigns(dir.path());
        let campaign = GameCampaign {
            name: "The Shattered Isles".into(),
            system: "5e".into(),
            party: vec!["Thoren Ironfist".into(), "Elara Moonwhisper".into()],
            npcs: vec!["Vasago".into(), "Queen Morwenna".into()],
            places: vec!["Ashfall".into(), "The Sundered Coast".into()],
            mechanics_extra: vec!["sorcery points".into(), "bardic inspiration".into()],
        };
        let slug = slugify(&campaign.name);
        let path = save_campaign(&config, &slug, &campaign).unwrap();
        assert!(path.exists());
        let loaded = load_campaign(&config, &slug).unwrap().unwrap();
        assert_eq!(loaded, campaign);
    }

    #[test]
    fn load_missing_campaign_returns_none() {
        let dir = TempDir::new().unwrap();
        let config = config_with_campaigns(dir.path());
        assert!(load_campaign(&config, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_campaigns_skips_malformed_files() {
        let dir = TempDir::new().unwrap();
        let config = config_with_campaigns(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("malformed.toml"), "this is not toml =").unwrap();
        std::fs::write(
            dir.path().join("good.toml"),
            r#"name = "Good"
system = "5e"
"#,
        )
        .unwrap();
        let summaries = list_campaigns(&config).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].slug, "good");
    }

    #[test]
    fn lexicon_for_known_systems() {
        let dnd = lexicon_for_system("5e");
        assert!(dnd.iter().any(|t| t == "Fireball"));
        assert!(dnd.iter().any(|t| t == "Strength"));

        let pf = lexicon_for_system("pf2e");
        assert!(pf.iter().any(|t| t == "Crafting"));

        let coc = lexicon_for_system("cocv7");
        assert!(coc.iter().any(|t| t == "Cthulhu Mythos"));

        let generic = lexicon_for_system("some-obscure-system");
        assert!(generic.iter().any(|t| t == "d20"));
    }

    #[test]
    fn validate_spans_accepts_full_coverage() {
        let spans = vec![
            SegmentSpan {
                first_line: 1,
                last_line: 5,
                class: SpanClass::InGame,
                reason: "".into(),
            },
            SegmentSpan {
                first_line: 6,
                last_line: 10,
                class: SpanClass::Banter,
                reason: "".into(),
            },
        ];
        assert!(validate_spans(&spans, 10).is_ok());
    }

    #[test]
    fn validate_spans_rejects_gaps() {
        let spans = vec![
            SegmentSpan {
                first_line: 1,
                last_line: 5,
                class: SpanClass::InGame,
                reason: "".into(),
            },
            SegmentSpan {
                first_line: 7,
                last_line: 10,
                class: SpanClass::Banter,
                reason: "".into(),
            },
        ];
        let err = validate_spans(&spans, 10).unwrap_err();
        assert!(err.contains("gap"));
    }

    #[test]
    fn validate_spans_rejects_overlaps() {
        let spans = vec![
            SegmentSpan {
                first_line: 1,
                last_line: 6,
                class: SpanClass::InGame,
                reason: "".into(),
            },
            SegmentSpan {
                first_line: 5,
                last_line: 10,
                class: SpanClass::Banter,
                reason: "".into(),
            },
        ];
        let err = validate_spans(&spans, 10).unwrap_err();
        assert!(err.contains("overlap"));
    }

    #[test]
    fn validate_spans_rejects_out_of_range() {
        let spans = vec![SegmentSpan {
            first_line: 1,
            last_line: 15,
            class: SpanClass::InGame,
            reason: "".into(),
        }];
        let err = validate_spans(&spans, 10).unwrap_err();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn validate_spans_rejects_trailing_gap() {
        let spans = vec![SegmentSpan {
            first_line: 1,
            last_line: 5,
            class: SpanClass::InGame,
            reason: "".into(),
        }];
        let err = validate_spans(&spans, 10).unwrap_err();
        assert!(err.contains("trailing"));
    }

    #[test]
    fn validate_spans_rejects_reversed_range() {
        let spans = vec![SegmentSpan {
            first_line: 5,
            last_line: 2,
            class: SpanClass::InGame,
            reason: "".into(),
        }];
        let err = validate_spans(&spans, 10).unwrap_err();
        assert!(err.contains("first_line"));
    }

    #[test]
    fn classifier_missing_binary_is_skipped() {
        let dir = TempDir::new().unwrap();
        let mut config = config_with_campaigns(dir.path());
        config.games.classifier.agent_binary =
            "/definitely/not/a/real/binary/xyz-minutes-test".into();
        config.games.classifier.agent_timeout_secs = 5;
        let outcome = classify_transcript(&config, "[0:00] Hello\n[0:01] World\n");
        match outcome {
            ClassifierOutcome::Skipped(reason) => {
                assert!(reason.contains("not found") || reason.contains("No such"));
            }
            other => panic!("expected Skipped, got {:?}", other),
        }
    }

    #[cfg(unix)]
    #[test]
    fn classifier_with_mock_agent_writes_spans() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        // Mock agent: a shell script that writes canned JSON via printf to
        // avoid depending on `cat` being on the restricted shell PATH.
        let script = dir.path().join("mock-classifier");
        let script_contents = "#!/bin/sh\nprintf '%s' '{\"spans\":[{\"first_line\":1,\"last_line\":1,\"class\":\"in_game\",\"reason\":\"DM narration\"},{\"first_line\":2,\"last_line\":2,\"class\":\"banter\",\"reason\":\"pizza\"}]}'\n";
        std::fs::write(&script, script_contents).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut config = config_with_campaigns(dir.path());
        config.games.classifier.agent_binary = script.display().to_string();
        config.games.classifier.agent_timeout_secs = 10;

        let outcome = classify_transcript(&config, "[0:00] Hello\n[0:01] World");
        match outcome {
            ClassifierOutcome::Ok(spans) => {
                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].class, SpanClass::InGame);
                assert_eq!(spans[1].class, SpanClass::Banter);
            }
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[cfg(unix)]
    #[test]
    fn classifier_rejects_bad_spans_and_gives_up_after_retry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("mock-bad-classifier");
        // Returns spans that don't cover every line — validator rejects. The
        // retry path will call the same script which will return the same
        // bad output; after two tries we expect `Rejected`.
        let script_contents = "#!/bin/sh\nprintf '%s' '{\"spans\":[{\"first_line\":1,\"last_line\":1,\"class\":\"in_game\",\"reason\":\"...\"}]}'\n";
        std::fs::write(&script, script_contents).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut config = config_with_campaigns(dir.path());
        config.games.classifier.agent_binary = script.display().to_string();
        config.games.classifier.agent_timeout_secs = 10;

        let outcome = classify_transcript(&config, "[0:00] Hello\n[0:01] World");
        match outcome {
            ClassifierOutcome::Rejected(reason) => {
                assert!(reason.contains("validation failed twice"));
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[test]
    fn priority_names_and_contextual_terms() {
        let campaign = GameCampaign {
            name: "Test".into(),
            system: "5e".into(),
            party: vec!["A".into()],
            npcs: vec!["B".into()],
            places: vec!["C".into()],
            mechanics_extra: vec!["D".into()],
        };
        assert_eq!(campaign.priority_names(), vec!["A", "B"]);
        assert_eq!(campaign.contextual_terms(), vec!["C", "D"]);
    }
}
