//! Content-safe meeting-transcript quality benchmark.
//!
//! The input may contain private reference and hypothesis text, but the output
//! contains aggregate metrics only. Neither text nor input/output paths are
//! printed. Commit only synthetic/public fixtures; keep private benchmark inputs
//! and reports outside the repository.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use murmur_lib::model_download::{
    build_stt_engine_for, model_file_matches_registry_pin, ModelRegistry, DEFAULT_STT_MODEL_ID,
};
use murmur_lib::stt::{SpeechGateMode, SttOptions, TimedTranscript};
use serde::{Deserialize, Serialize};

const FIXTURE_VERSION: u32 = 1;
const RUN_MANIFEST_VERSION: u32 = 1;
const ACTIVITY_MANIFEST_VERSION: u32 = 1;
const MATCH_TIME_TOLERANCE_MS: u64 = 2_000;
const MATCH_MIN_TOKEN_RECALL: f64 = 0.6;
const BLEED_WINDOW_MS: u64 = 3_000;
const BLEED_MIN_TOKEN_CONTAINMENT: f64 = 0.8;
const REPEAT_WINDOW_MS: u64 = 2_000;
// Report the production meeting-microphone policy, not the historical five-frame screen.
const MEETING_MICROPHONE_GATE_POLICY: &str = "silero-v6-sha256:597d30b3ec076608d059477bb14cfeffdf951bf5cae370d38f65d33bbfe82004;ort-1.28.0;onset=0.50;offset=0.35;positive_frames=6;initial_context_ms=300;minimum_real_region_ms=1700";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum Track {
    Microphone,
    System,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RegionKind {
    Speech,
    Acknowledgment,
    ShortTurn,
    Overlap,
    ContextualTerm,
    GarbledPassage,
}

fn default_region_kind() -> RegionKind {
    RegionKind::Speech
}

#[derive(Debug, Clone, Deserialize)]
struct Segment {
    id: String,
    track: Track,
    start_ms: u64,
    end_ms: u64,
    text: String,
    #[serde(default = "default_region_kind")]
    kind: RegionKind,
}

#[derive(Debug, Clone, Deserialize)]
struct KeyTerm {
    id: String,
    text: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    version: u32,
    fixture_id: String,
    reference_segments: Vec<Segment>,
    hypothesis_segments: Vec<Segment>,
    #[serde(default)]
    key_terms: Vec<KeyTerm>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct QualityReport {
    fixture_version: u32,
    fixture_id: String,
    reference_segments: usize,
    hypothesis_segments: usize,
    reference_words: usize,
    hypothesis_words: usize,
    word_error_rate: f64,
    missing_regions: usize,
    repeated_segments: usize,
    timestamp_order_violations: usize,
    matched_timestamp_errors_over_2s: usize,
    acknowledgment_regions: usize,
    missing_acknowledgments: usize,
    short_turn_regions: usize,
    missing_short_turns: usize,
    overlap_regions: usize,
    missing_overlaps: usize,
    key_terms: usize,
    missing_key_terms: usize,
    key_term_error_rate: f64,
    speaker_bleed_candidates: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline: Option<PipelineMetrics>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PipelineMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speech_gate: Option<String>,
    stt_model_id: String,
    shipping_model_sha256_verified: bool,
    audio_sources: usize,
    microphone_audio_sources: usize,
    system_audio_sources: usize,
    total_audio_duration_ms: u64,
    inference_duration_ms: u64,
    process_peak_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunManifest {
    version: u32,
    corpus_id: String,
    consent_confirmed: bool,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    initial_prompt: Option<String>,
    audio_sources: Vec<AudioSource>,
    reference_segments: Vec<Segment>,
    #[serde(default)]
    key_terms: Vec<KeyTerm>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioSource {
    id: String,
    track: Track,
    wav_path: PathBuf,
    timeline_start_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityManifest {
    version: u32,
    corpus_id: String,
    consent_confirmed: bool,
    audio_sources: Vec<AudioSource>,
    windows: Vec<ActivityWindow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityWindow {
    source_id: String,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ActivityReport {
    windows: Vec<ActivityWindowReport>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ActivityWindowReport {
    track: Track,
    start_ms: u64,
    end_ms: u64,
    duration_ms: u64,
    frame_count: u64,
    sample_count: u64,
    peak: f64,
    rms: f64,
}

enum Mode {
    Score {
        input: PathBuf,
        output: Option<PathBuf>,
    },
    Run {
        manifest: PathBuf,
        model: Option<PathBuf>,
        output: PathBuf,
        meeting_microphone_gate: bool,
    },
    Activity {
        manifest: PathBuf,
        output: PathBuf,
    },
}

struct Args {
    mode: Mode,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut input = None;
    let mut manifest = None;
    let mut activity_manifest = None;
    let mut model = None;
    let mut output = None;
    let mut meeting_microphone_gate = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut next_path = || {
            args.next()
                .map(PathBuf::from)
                .ok_or_else(|| "argument value is missing".to_owned())
        };
        match arg.as_str() {
            "--input" => input = Some(next_path()?),
            "--manifest" => manifest = Some(next_path()?),
            "--activity-manifest" => activity_manifest = Some(next_path()?),
            "--model" => model = Some(next_path()?),
            "--output" => output = Some(next_path()?),
            "--meeting-microphone-gate" => meeting_microphone_gate = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => return Err("unknown argument".to_owned()),
        }
    }
    let mode = match (input, manifest, activity_manifest) {
        (Some(input), None, None) if model.is_none() && !meeting_microphone_gate => {
            Mode::Score { input, output }
        }
        (None, Some(manifest), None) => Mode::Run {
            manifest,
            model,
            output: output.ok_or_else(|| "--output is required with --manifest".to_owned())?,
            meeting_microphone_gate,
        },
        (None, None, Some(manifest)) if model.is_none() && !meeting_microphone_gate => {
            Mode::Activity {
                manifest,
                output: output
                    .ok_or_else(|| "--output is required with --activity-manifest".to_owned())?,
            }
        }
        _ => {
            return Err(
                "choose exactly one of --input, --manifest, or --activity-manifest".to_owned(),
            )
        }
    };
    Ok(Args { mode })
}

fn print_usage() {
    eprintln!(
        "usage:\n  cargo run --example meeting_transcript_quality_bench -- \\\n         --input <fixture.json> [--output <aggregate-report.json>]\n  \\\n         cargo run --quiet --release --example meeting_transcript_quality_bench -- \\\n         --manifest <private-manifest.json> [--model <ggml-model>] [--meeting-microphone-gate] \\\n         --output <new-aggregate-report.json>\n  \\\n         cargo run --quiet --example meeting_transcript_quality_bench -- \\\n         --activity-manifest <private-activity-manifest.json> \\\n         --output <new-aggregate-activity-report.json>"
    );
}

#[tokio::main]
async fn main() {
    let result = match parse_args() {
        Ok(args) => run(args).await,
        Err(error) => {
            eprintln!("error: {error}");
            print_usage();
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

async fn run(args: Args) -> Result<(), String> {
    match args.mode {
        Mode::Score { input, output } => run_score_mode(&input, output.as_deref()),
        Mode::Run {
            manifest,
            model,
            output,
            meeting_microphone_gate,
        } => run_timed_whisper_mode(&manifest, model, &output, meeting_microphone_gate).await,
        Mode::Activity { manifest, output } => run_activity_mode(&manifest, &output),
    }
}

fn run_score_mode(input: &Path, output: Option<&Path>) -> Result<(), String> {
    let raw =
        fs::read_to_string(input).map_err(|_| "benchmark input could not be read".to_owned())?;
    let fixture: Fixture =
        serde_json::from_str(&raw).map_err(|_| "benchmark input is invalid JSON".to_owned())?;
    let report =
        score(&fixture).map_err(|error| format!("benchmark input failed validation: {error}"))?;
    write_report(&report, output)
}

fn write_report(report: &impl Serialize, output: Option<&Path>) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(report)
        .map_err(|_| "aggregate report could not be serialized".to_owned())?;
    if let Some(path) = output {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| "aggregate report could not be created".to_owned())?;
        file.write_all(format!("{serialized}\n").as_bytes())
            .map_err(|_| "aggregate report could not be written".to_owned())?;
        eprintln!("[meeting-quality] aggregate report written");
    } else {
        println!("{serialized}");
    }
    Ok(())
}

fn run_activity_mode(manifest_path: &Path, output_path: &Path) -> Result<(), String> {
    let repository = repository_root()?;
    let manifest_path = canonical_private_input(manifest_path, &repository)?;
    ensure_private_output(output_path, &repository)?;

    let raw = fs::read_to_string(&manifest_path)
        .map_err(|_| "private activity manifest could not be read".to_owned())?;
    let manifest: ActivityManifest = serde_json::from_str(&raw)
        .map_err(|_| "private activity manifest is invalid JSON".to_owned())?;
    validate_activity_manifest(&manifest)?;
    let audio_paths = resolve_audio_paths(&manifest.audio_sources, &manifest_path, &repository)?;

    let mut windows = Vec::with_capacity(manifest.windows.len());
    for window in &manifest.windows {
        let source_index = manifest
            .audio_sources
            .iter()
            .position(|source| source.id == window.source_id)
            .ok_or_else(|| "activity window source is unavailable".to_owned())?;
        windows.push(measure_activity_window(
            &audio_paths[source_index],
            &manifest.audio_sources[source_index],
            window,
        )?);
    }

    write_report(&ActivityReport { windows }, Some(output_path))
}

fn validate_activity_manifest(manifest: &ActivityManifest) -> Result<(), String> {
    if manifest.version != ACTIVITY_MANIFEST_VERSION {
        return Err("unsupported private activity manifest version".to_owned());
    }
    if !manifest.consent_confirmed {
        return Err("explicit corpus-owner consent must be confirmed".to_owned());
    }
    if !is_content_safe_label(&manifest.corpus_id) {
        return Err("corpus_id must be a content-safe label".to_owned());
    }
    if manifest.audio_sources.is_empty() || manifest.windows.is_empty() {
        return Err("activity manifest sources and windows must not be empty".to_owned());
    }

    let mut source_ids = HashSet::new();
    for source in &manifest.audio_sources {
        if !is_content_safe_label(&source.id) || !source_ids.insert(source.id.as_str()) {
            return Err("audio source metadata is invalid".to_owned());
        }
    }
    if manifest.windows.iter().any(|window| {
        window.end_ms <= window.start_ms || !source_ids.contains(window.source_id.as_str())
    }) {
        return Err("activity window metadata is invalid".to_owned());
    }
    Ok(())
}

fn measure_activity_window(
    path: &Path,
    source: &AudioSource,
    window: &ActivityWindow,
) -> Result<ActivityWindowReport, String> {
    let local_start_ms = window
        .start_ms
        .checked_sub(source.timeline_start_ms)
        .ok_or_else(|| "activity window precedes its source timeline".to_owned())?;
    let local_end_ms = window
        .end_ms
        .checked_sub(source.timeline_start_ms)
        .ok_or_else(|| "activity window precedes its source timeline".to_owned())?;
    let mut reader = hound::WavReader::open(path)
        .map_err(|_| "private activity WAV could not be read".to_owned())?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample == 0
        || spec.bits_per_sample > 32
        || spec.channels == 0
        || spec.sample_rate == 0
    {
        return Err("private activity WAV format is unsupported".to_owned());
    }

    let sample_rate = u64::from(spec.sample_rate);
    let start_frame = local_start_ms
        .checked_mul(sample_rate)
        .ok_or_else(|| "activity window frame range overflowed".to_owned())?
        / 1_000;
    let end_numerator = local_end_ms
        .checked_mul(sample_rate)
        .and_then(|value| value.checked_add(999))
        .ok_or_else(|| "activity window frame range overflowed".to_owned())?;
    let end_frame = end_numerator / 1_000;
    let total_frames = u64::from(reader.duration());
    if start_frame >= end_frame || end_frame > total_frames {
        return Err("activity window is outside its WAV source".to_owned());
    }

    let channels = u64::from(spec.channels);
    let first_sample = start_frame
        .checked_mul(channels)
        .ok_or_else(|| "activity window sample range overflowed".to_owned())?;
    let sample_count = end_frame
        .checked_sub(start_frame)
        .and_then(|frames| frames.checked_mul(channels))
        .ok_or_else(|| "activity window sample range overflowed".to_owned())?;
    let last_sample = first_sample
        .checked_add(sample_count)
        .ok_or_else(|| "activity window sample range overflowed".to_owned())?;
    let full_scale = if spec.bits_per_sample >= 32 {
        i32::MAX as f64
    } else {
        ((1_u64 << (spec.bits_per_sample - 1)) - 1) as f64
    };

    let mut measured_samples = 0_u64;
    let mut peak = 0_f64;
    let mut sum_squares = 0_f64;
    for (index, sample) in reader.samples::<i32>().enumerate() {
        let index =
            u64::try_from(index).map_err(|_| "activity WAV sample index overflowed".to_owned())?;
        if index < first_sample {
            continue;
        }
        if index >= last_sample {
            break;
        }
        let normalized =
            f64::from(sample.map_err(|_| "private activity WAV could not be decoded".to_owned())?)
                / full_scale;
        peak = peak.max(normalized.abs());
        sum_squares += normalized * normalized;
        measured_samples = measured_samples.saturating_add(1);
    }
    if measured_samples != sample_count {
        return Err("private activity WAV ended before its declared window".to_owned());
    }
    let rms = (sum_squares / measured_samples as f64).sqrt();

    Ok(ActivityWindowReport {
        track: source.track,
        start_ms: window.start_ms,
        end_ms: window.end_ms,
        duration_ms: window.end_ms - window.start_ms,
        frame_count: end_frame - start_frame,
        sample_count,
        peak,
        rms,
    })
}

fn score(fixture: &Fixture) -> Result<QualityReport, String> {
    validate_fixture(fixture)?;

    let reference_tokens = flattened_tokens(&fixture.reference_segments);
    let hypothesis_tokens = flattened_tokens(&fixture.hypothesis_segments);
    let word_error_rate = if reference_tokens.is_empty() {
        if hypothesis_tokens.is_empty() {
            0.0
        } else {
            1.0
        }
    } else {
        levenshtein(&reference_tokens, &hypothesis_tokens) as f64 / reference_tokens.len() as f64
    };

    let matches: Vec<Option<usize>> = fixture
        .reference_segments
        .iter()
        .map(|reference| best_match(reference, &fixture.hypothesis_segments))
        .collect();
    let missing_regions = matches.iter().filter(|matched| matched.is_none()).count();

    let missing_by_kind = |kind| {
        fixture
            .reference_segments
            .iter()
            .zip(&matches)
            .filter(|(segment, matched)| segment.kind == kind && matched.is_none())
            .count()
    };
    let count_kind = |kind| {
        fixture
            .reference_segments
            .iter()
            .filter(|segment| segment.kind == kind)
            .count()
    };

    let matched_timestamp_errors_over_2s = fixture
        .reference_segments
        .iter()
        .zip(&matches)
        .filter(|(reference, matched)| {
            matched.is_some_and(|index| {
                reference
                    .start_ms
                    .abs_diff(fixture.hypothesis_segments[index].start_ms)
                    > MATCH_TIME_TOLERANCE_MS
            })
        })
        .count();

    let hypothesis_text = normalized_join(&fixture.hypothesis_segments);
    let missing_key_terms = fixture
        .key_terms
        .iter()
        .filter(|term| !contains_phrase(&hypothesis_text, &normalize(&term.text)))
        .count();
    let key_term_error_rate = if fixture.key_terms.is_empty() {
        0.0
    } else {
        missing_key_terms as f64 / fixture.key_terms.len() as f64
    };

    Ok(QualityReport {
        fixture_version: fixture.version,
        fixture_id: fixture.fixture_id.clone(),
        reference_segments: fixture.reference_segments.len(),
        hypothesis_segments: fixture.hypothesis_segments.len(),
        reference_words: reference_tokens.len(),
        hypothesis_words: hypothesis_tokens.len(),
        word_error_rate,
        missing_regions,
        repeated_segments: repeated_segments(&fixture.hypothesis_segments),
        timestamp_order_violations: timestamp_order_violations(&fixture.hypothesis_segments),
        matched_timestamp_errors_over_2s,
        acknowledgment_regions: count_kind(RegionKind::Acknowledgment),
        missing_acknowledgments: missing_by_kind(RegionKind::Acknowledgment),
        short_turn_regions: count_kind(RegionKind::ShortTurn),
        missing_short_turns: missing_by_kind(RegionKind::ShortTurn),
        overlap_regions: count_kind(RegionKind::Overlap),
        missing_overlaps: missing_by_kind(RegionKind::Overlap),
        key_terms: fixture.key_terms.len(),
        missing_key_terms,
        key_term_error_rate,
        speaker_bleed_candidates: speaker_bleed_candidates(&fixture.hypothesis_segments),
        pipeline: None,
    })
}

fn validate_fixture(fixture: &Fixture) -> Result<(), String> {
    if fixture.version != FIXTURE_VERSION {
        return Err("unsupported fixture version".to_owned());
    }
    if fixture.fixture_id.is_empty()
        || !fixture
            .fixture_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err("fixture_id must be a content-safe label".to_owned());
    }
    if fixture.reference_segments.is_empty() {
        return Err("reference_segments must not be empty".to_owned());
    }
    let mut segment_ids = HashSet::new();
    for segment in fixture
        .reference_segments
        .iter()
        .chain(&fixture.hypothesis_segments)
    {
        if segment.id.trim().is_empty()
            || segment.text.trim().is_empty()
            || segment.end_ms < segment.start_ms
            || !segment_ids.insert(segment.id.as_str())
        {
            return Err("segment metadata is invalid".to_owned());
        }
    }
    let mut term_ids = HashSet::new();
    for term in &fixture.key_terms {
        if term.id.trim().is_empty()
            || normalize(&term.text).is_empty()
            || !term_ids.insert(term.id.as_str())
        {
            return Err("key-term metadata is invalid".to_owned());
        }
    }
    Ok(())
}

async fn run_timed_whisper_mode(
    manifest_path: &Path,
    model_path: Option<PathBuf>,
    output_path: &Path,
    meeting_microphone_gate: bool,
) -> Result<(), String> {
    let repository = repository_root()?;
    let manifest_path = canonical_private_input(manifest_path, &repository)?;
    ensure_private_output(output_path, &repository)?;

    let raw = fs::read_to_string(&manifest_path)
        .map_err(|_| "private manifest could not be read".to_owned())?;
    let manifest: RunManifest =
        serde_json::from_str(&raw).map_err(|_| "private manifest is invalid JSON".to_owned())?;
    validate_run_manifest(&manifest)?;
    let audio_paths = resolve_audio_paths(&manifest.audio_sources, &manifest_path, &repository)?;

    let model_path = model_path
        .or_else(|| std::env::var_os(murmur_lib::MODEL_PATH_ENV).map(PathBuf::from))
        .ok_or_else(|| {
            "shipping Whisper model must be supplied with --model or MURMUR_WHISPER_MODEL_PATH"
                .to_owned()
        })?;
    let model_path = canonical_model_path(&model_path)?;
    let model = ModelRegistry::get(DEFAULT_STT_MODEL_ID)
        .ok_or_else(|| "shipping STT registry entry is unavailable".to_owned())?;
    let model_matches = model_file_matches_registry_pin(&model_path, model)
        .map_err(|_| "Whisper model could not be verified".to_owned())?;
    if !model_matches {
        return Err("Whisper model does not match the shipping registry pin".to_owned());
    }

    // whisper.cpp normally logs its model path while loading. Route native logs
    // through the Rust `log` facade; this benchmark installs no logger, so model
    // and fixture paths cannot escape on stdout/stderr.
    whisper_rs::install_whisper_log_trampoline();
    // The benchmark path is an explicit developer input outside app-managed
    // storage; it was verified above but remains read-only/unmanaged.
    let engine = build_stt_engine_for(model, model_path, false);
    if engine.id() != "whisper" {
        return Err("shipping STT engine is not Whisper".to_owned());
    }
    let options = SttOptions {
        language: manifest.language.clone(),
        initial_prompt: if meeting_microphone_gate {
            None
        } else {
            manifest.initial_prompt.clone()
        },
        translate: false,
        cancellation: None,
        ..SttOptions::default()
    };

    let mut transcripts = Vec::with_capacity(audio_paths.len());
    let mut inference_duration_ms = 0_u64;
    let mut total_audio_duration_ms = 0_u64;
    for (source, audio_path) in manifest.audio_sources.iter().zip(&audio_paths) {
        let started = Instant::now();
        let mut track_options = options.clone();
        if meeting_microphone_gate && source.track == Track::Microphone {
            track_options.speech_gate = SpeechGateMode::MeetingMicrophone;
        }
        let result = engine.transcribe_timed(audio_path, &track_options).await;
        inference_duration_ms = inference_duration_ms.saturating_add(duration_ms(started));
        // Meeting processing unloads after every attempted unit inference. Keep
        // the benchmark on that same lifecycle rather than retaining a warmer
        // context that the shipping meeting path does not retain.
        engine.unload().await;
        let transcript = result.map_err(|_| "timed Whisper inference failed".to_owned())?;
        total_audio_duration_ms = total_audio_duration_ms
            .checked_add(u64::from(transcript.duration_ms))
            .ok_or_else(|| "aggregate audio duration overflowed".to_owned())?;
        transcripts.push(transcript);
    }

    let hypotheses = hypothesis_segments_from_native(&manifest.audio_sources, &transcripts)?;
    let fixture = Fixture {
        version: FIXTURE_VERSION,
        fixture_id: manifest.corpus_id.clone(),
        reference_segments: manifest.reference_segments.clone(),
        hypothesis_segments: hypotheses,
        key_terms: manifest.key_terms.clone(),
    };
    let mut report =
        score(&fixture).map_err(|_| "generated benchmark timeline failed validation".to_owned())?;
    report.pipeline = Some(PipelineMetrics {
        speech_gate: meeting_microphone_gate.then(|| MEETING_MICROPHONE_GATE_POLICY.to_owned()),
        stt_model_id: model.id.to_owned(),
        shipping_model_sha256_verified: true,
        audio_sources: manifest.audio_sources.len(),
        microphone_audio_sources: manifest
            .audio_sources
            .iter()
            .filter(|source| source.track == Track::Microphone)
            .count(),
        system_audio_sources: manifest
            .audio_sources
            .iter()
            .filter(|source| source.track == Track::System)
            .count(),
        total_audio_duration_ms,
        inference_duration_ms,
        process_peak_rss_bytes: peak_rss_bytes(),
    });
    write_report(&report, Some(output_path))
}

fn validate_run_manifest(manifest: &RunManifest) -> Result<(), String> {
    if manifest.version != RUN_MANIFEST_VERSION {
        return Err("unsupported private manifest version".to_owned());
    }
    if !manifest.consent_confirmed {
        return Err("explicit corpus-owner consent must be confirmed".to_owned());
    }
    if manifest.audio_sources.is_empty() {
        return Err("audio_sources must not be empty".to_owned());
    }
    if manifest
        .language
        .as_ref()
        .is_some_and(|language| language.trim().is_empty())
    {
        return Err("language must be omitted or non-empty".to_owned());
    }

    let fixture = Fixture {
        version: FIXTURE_VERSION,
        fixture_id: manifest.corpus_id.clone(),
        reference_segments: manifest.reference_segments.clone(),
        hypothesis_segments: Vec::new(),
        key_terms: manifest.key_terms.clone(),
    };
    validate_fixture(&fixture)?;

    let mut source_ids = HashSet::new();
    let mut source_tracks = HashSet::new();
    for source in &manifest.audio_sources {
        if !is_content_safe_label(&source.id) || !source_ids.insert(source.id.as_str()) {
            return Err("audio source metadata is invalid".to_owned());
        }
        source_tracks.insert(source.track);
    }
    if manifest
        .reference_segments
        .iter()
        .any(|segment| !source_tracks.contains(&segment.track))
    {
        return Err("reference track has no matching audio source".to_owned());
    }
    Ok(())
}

fn is_content_safe_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn repository_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .ok_or_else(|| "repository boundary is unavailable".to_owned())?;
    fs::canonicalize(root).map_err(|_| "repository boundary is unavailable".to_owned())
}

fn canonical_private_input(path: &Path, repository: &Path) -> Result<PathBuf, String> {
    let canonical =
        fs::canonicalize(path).map_err(|_| "private manifest could not be resolved".to_owned())?;
    let metadata = fs::metadata(&canonical)
        .map_err(|_| "private manifest could not be inspected".to_owned())?;
    if !metadata.is_file() || canonical.starts_with(repository) {
        return Err("private manifest must be a file outside the repository".to_owned());
    }
    Ok(canonical)
}

fn ensure_private_output(path: &Path, repository: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("aggregate report path must be absolute".to_owned());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "aggregate report parent is unavailable".to_owned())?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|_| "aggregate report parent could not be resolved".to_owned())?;
    if canonical_parent.starts_with(repository) {
        return Err("aggregate report must remain outside the repository".to_owned());
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err("aggregate report target must not already exist".to_owned()),
        Err(_) => Err("aggregate report target could not be inspected".to_owned()),
    }
}

fn resolve_audio_paths(
    sources: &[AudioSource],
    manifest_path: &Path,
    repository: &Path,
) -> Result<Vec<PathBuf>, String> {
    let base = manifest_path
        .parent()
        .ok_or_else(|| "private manifest parent is unavailable".to_owned())?;
    sources
        .iter()
        .map(|source| {
            let unresolved = if source.wav_path.is_absolute() {
                source.wav_path.clone()
            } else {
                base.join(&source.wav_path)
            };
            let canonical = fs::canonicalize(unresolved)
                .map_err(|_| "private WAV could not be resolved".to_owned())?;
            let is_wav = canonical
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"));
            let is_file = fs::metadata(&canonical)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false);
            if !is_wav || !is_file || canonical.starts_with(repository) {
                return Err("private WAV must be a WAV file outside the repository".to_owned());
            }
            Ok(canonical)
        })
        .collect()
}

fn canonical_model_path(path: &Path) -> Result<PathBuf, String> {
    let canonical =
        fs::canonicalize(path).map_err(|_| "Whisper model could not be resolved".to_owned())?;
    if !fs::metadata(&canonical)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return Err("Whisper model is not a regular file".to_owned());
    }
    Ok(canonical)
}

fn duration_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn hypothesis_segments_from_native(
    sources: &[AudioSource],
    transcripts: &[TimedTranscript],
) -> Result<Vec<Segment>, String> {
    if sources.len() != transcripts.len() {
        return Err("audio source/transcript count mismatch".to_owned());
    }
    let mut hypotheses = Vec::new();
    for (source_index, (source, transcript)) in sources.iter().zip(transcripts).enumerate() {
        let mut previous_start = None;
        for (segment_index, native) in transcript.segments.iter().enumerate() {
            if native.end_ms < native.start_ms
                || previous_start.is_some_and(|start| native.start_ms < start)
            {
                return Err("native Whisper segment timing is invalid".to_owned());
            }
            previous_start = Some(native.start_ms);
            if native.text.trim().is_empty() {
                continue;
            }
            let start_ms = source
                .timeline_start_ms
                .checked_add(u64::from(native.start_ms))
                .ok_or_else(|| "derived benchmark timestamp overflowed".to_owned())?;
            let end_ms = source
                .timeline_start_ms
                .checked_add(u64::from(native.end_ms))
                .ok_or_else(|| "derived benchmark timestamp overflowed".to_owned())?;
            hypotheses.push(Segment {
                id: format!("__murmur_hypothesis_{source_index}_{segment_index}"),
                track: source.track,
                start_ms,
                end_ms,
                text: native.text.clone(),
                kind: RegionKind::Speech,
            });
        }
    }
    hypotheses.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then_with(|| left.end_ms.cmp(&right.end_ms))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(hypotheses)
}

/// Process high-water RSS in bytes. `getrusage` is lifetime-cumulative;
/// macOS reports bytes while other Unix targets report KiB.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        let raw = usage.ru_maxrss as u64;
        #[cfg(target_os = "macos")]
        {
            Some(raw)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(raw.saturating_mul(1024))
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn flattened_tokens(segments: &[Segment]) -> Vec<String> {
    segments
        .iter()
        .flat_map(|segment| tokens(&segment.text))
        .collect()
}

fn normalized_join(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| normalize(&segment.text))
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '\'' {
            normalized.push(character);
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokens(text: &str) -> Vec<String> {
    normalize(text)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn contains_phrase(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let padded_haystack = format!(" {haystack} ");
    let padded_needle = format!(" {needle} ");
    padded_haystack.contains(&padded_needle)
}

fn best_match(reference: &Segment, hypothesis: &[Segment]) -> Option<usize> {
    let reference_tokens = tokens(&reference.text);
    hypothesis
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.track == reference.track
                && intervals_are_near(reference, candidate, MATCH_TIME_TOLERANCE_MS)
        })
        .map(|(index, candidate)| {
            (
                index,
                token_recall(&reference_tokens, &tokens(&candidate.text)),
                reference.start_ms.abs_diff(candidate.start_ms),
            )
        })
        .filter(|(_, recall, _)| *recall >= MATCH_MIN_TOKEN_RECALL)
        .max_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| right.2.cmp(&left.2))
        })
        .map(|(index, _, _)| index)
}

fn intervals_are_near(left: &Segment, right: &Segment, tolerance_ms: u64) -> bool {
    left.start_ms <= right.end_ms.saturating_add(tolerance_ms)
        && right.start_ms <= left.end_ms.saturating_add(tolerance_ms)
}

fn token_recall(reference: &[String], hypothesis: &[String]) -> f64 {
    if reference.is_empty() {
        return if hypothesis.is_empty() { 1.0 } else { 0.0 };
    }
    let mut available = token_counts(hypothesis);
    let retained = reference
        .iter()
        .filter(|token| {
            available.get_mut(token.as_str()).is_some_and(|count| {
                if *count == 0 {
                    false
                } else {
                    *count -= 1;
                    true
                }
            })
        })
        .count();
    retained as f64 / reference.len() as f64
}

fn token_counts(tokens: &[String]) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for token in tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    counts
}

fn timestamp_order_violations(segments: &[Segment]) -> usize {
    segments
        .windows(2)
        .filter(|pair| pair[1].start_ms < pair[0].start_ms)
        .count()
}

fn repeated_segments(segments: &[Segment]) -> usize {
    segments
        .windows(2)
        .filter(|pair| {
            pair[0].track == pair[1].track
                && pair[1].start_ms.abs_diff(pair[0].end_ms) <= REPEAT_WINDOW_MS
                && normalize(&pair[0].text) == normalize(&pair[1].text)
        })
        .count()
}

fn speaker_bleed_candidates(segments: &[Segment]) -> usize {
    let mut candidates = 0;
    for (index, left) in segments.iter().enumerate() {
        let left_tokens = tokens(&left.text);
        for right in &segments[index + 1..] {
            if left.track == right.track
                || !intervals_are_near(left, right, BLEED_WINDOW_MS)
                || left_tokens.is_empty()
            {
                continue;
            }
            let right_tokens = tokens(&right.text);
            if right_tokens.is_empty() {
                continue;
            }
            let shorter = left_tokens.len().min(right_tokens.len());
            let retained = multiset_intersection(&left_tokens, &right_tokens);
            if retained as f64 / shorter as f64 >= BLEED_MIN_TOKEN_CONTAINMENT {
                candidates += 1;
            }
        }
    }
    candidates
}

fn multiset_intersection(left: &[String], right: &[String]) -> usize {
    let mut available = token_counts(right);
    left.iter()
        .filter(|token| {
            available.get_mut(token.as_str()).is_some_and(|count| {
                if *count == 0 {
                    false
                } else {
                    *count -= 1;
                    true
                }
            })
        })
        .count()
}

fn levenshtein(left: &[String], right: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_token) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_token) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_token != right_token);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current[right_index + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn synthetic_fixture() -> Fixture {
        serde_json::from_str(include_str!(
            "../benches/meeting_transcript_quality/synthetic-regression.json"
        ))
        .expect("synthetic fixture parses")
    }

    #[test]
    fn missing_short_turn_can_mean_wrong_word_not_missing_audio_region() {
        // Fictional text: characterize the metric, not the private/public replay.
        let reference = Segment {
            id: "reference".into(),
            track: Track::Microphone,
            start_ms: 10_000,
            end_ms: 10_700,
            text: "go".into(),
            kind: RegionKind::ShortTurn,
        };
        let hypothesis = Segment {
            id: "hypothesis".into(),
            kind: RegionKind::Speech,
            ..reference.clone()
        };
        let mut fixture = Fixture {
            version: FIXTURE_VERSION,
            fixture_id: "metric-characterization".into(),
            reference_segments: vec![reference],
            hypothesis_segments: vec![hypothesis],
            key_terms: vec![],
        };
        let baseline = score(&fixture).unwrap();
        fixture.hypothesis_segments[0].text = "stop".into();
        let substituted = score(&fixture).unwrap();
        assert_eq!(
            baseline.hypothesis_segments,
            substituted.hypothesis_segments
        );
        assert_eq!(baseline.missing_short_turns, 0);
        assert_eq!(substituted.missing_short_turns, 1);
        assert_eq!(baseline.word_error_rate, 0.0);
        assert_eq!(substituted.word_error_rate, 1.0);
        // This counter only examines matched text, so zero does not exclude a
        // timestamp-caused non-match elsewhere in a real quality report.
        assert_eq!(substituted.matched_timestamp_errors_over_2s, 0);
    }

    #[test]
    fn nearby_same_word_references_can_reuse_a_hypothesis_but_spaced_ones_cannot() {
        // Document the existing scorer limitation; do not silently change the
        // metric underlying historical reports. Close/overlapping real turns
        // need source-aware matching before they can support recall claims.
        let reference = Segment {
            id: "first".into(),
            track: Track::Microphone,
            start_ms: 0,
            end_ms: 700,
            text: "yes".into(),
            kind: RegionKind::Acknowledgment,
        };
        let mut fixture = Fixture {
            version: FIXTURE_VERSION,
            fixture_id: "spacing-characterization".into(),
            reference_segments: vec![
                reference.clone(),
                Segment {
                    id: "second".into(),
                    start_ms: 2000,
                    end_ms: 2700,
                    ..reference.clone()
                },
            ],
            hypothesis_segments: vec![Segment {
                id: "hypothesis".into(),
                kind: RegionKind::Speech,
                ..reference
            }],
            key_terms: vec![],
        };
        assert_eq!(score(&fixture).unwrap().missing_acknowledgments, 0);
        fixture.reference_segments[1].start_ms = 10_000;
        fixture.reference_segments[1].end_ms = 10_700;
        assert_eq!(score(&fixture).unwrap().missing_acknowledgments, 1);
    }

    fn temp_activity_wav(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-meeting-activity-{name}-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ))
    }

    fn write_activity_fixture(path: &Path) {
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 1_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .expect("synthetic activity WAV is created");
        for frame in 0..34_000 {
            let sample = if (29_600..30_000).contains(&frame) {
                16_384_i16
            } else if (32_000..33_000).contains(&frame) {
                1_i16
            } else {
                0_i16
            };
            writer
                .write_sample(sample)
                .expect("synthetic activity sample is written");
        }
        writer.finalize().expect("synthetic activity WAV closes");
    }

    fn activity_source(path: PathBuf) -> AudioSource {
        AudioSource {
            id: "source-a".to_owned(),
            track: Track::Microphone,
            wav_path: path,
            timeline_start_ms: 0,
        }
    }

    fn activity_window(start_ms: u64, end_ms: u64) -> ActivityWindow {
        ActivityWindow {
            source_id: "source-a".to_owned(),
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn synthetic_fixture_exercises_every_outstanding_quality_boundary() {
        let fixture = synthetic_fixture();
        let report = score(&fixture).expect("fixture scores");

        assert_eq!(report.fixture_id, "synthetic-meeting-quality-v1");
        assert_eq!(report.reference_segments, 10);
        assert_eq!(report.hypothesis_segments, 10);
        assert_eq!(report.missing_regions, 1);
        assert_eq!(report.repeated_segments, 1);
        assert_eq!(report.timestamp_order_violations, 0);
        assert_eq!(report.acknowledgment_regions, 1);
        assert_eq!(report.missing_acknowledgments, 0);
        assert_eq!(report.short_turn_regions, 1);
        assert_eq!(report.missing_short_turns, 0);
        assert_eq!(report.overlap_regions, 1);
        assert_eq!(report.missing_overlaps, 0);
        assert_eq!(report.key_terms, 4);
        assert_eq!(report.missing_key_terms, 2);
        assert_eq!(report.key_term_error_rate, 0.5);
        assert_eq!(report.speaker_bleed_candidates, 1);
        assert!(report.word_error_rate > 0.0);
    }

    #[test]
    fn scoring_never_mutates_or_removes_fixture_text() {
        let fixture = synthetic_fixture();
        let before = fixture
            .reference_segments
            .iter()
            .chain(&fixture.hypothesis_segments)
            .map(|segment| segment.text.clone())
            .collect::<Vec<_>>();

        let _ = score(&fixture).expect("fixture scores");

        let after = fixture
            .reference_segments
            .iter()
            .chain(&fixture.hypothesis_segments)
            .map(|segment| segment.text.clone())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
    }

    #[test]
    fn fixture_labels_cannot_contain_private_paths_or_freeform_text() {
        let mut fixture = synthetic_fixture();
        fixture.fixture_id = "unsafe label/with spaces".to_owned();
        assert_eq!(
            score(&fixture).unwrap_err(),
            "fixture_id must be a content-safe label"
        );
    }

    #[test]
    fn private_manifest_template_is_parseable_but_requires_explicit_consent() {
        let manifest: RunManifest = serde_json::from_str(include_str!(
            "../benches/meeting_transcript_quality/private-manifest.template.json"
        ))
        .expect("private manifest template parses");

        assert!(!manifest.consent_confirmed);
        assert_eq!(
            validate_run_manifest(&manifest).unwrap_err(),
            "explicit corpus-owner consent must be confirmed"
        );
        let mut consented = manifest;
        consented.consent_confirmed = true;
        validate_run_manifest(&consented).expect("consented template metadata validates");
    }

    #[test]
    fn path_validation_errors_never_echo_private_paths() {
        let repository = repository_root().expect("repository root resolves");
        let private_name = "participant-name-private-meeting.wav";
        let error = canonical_private_input(Path::new(private_name), &repository)
            .expect_err("missing private input is rejected");
        assert!(!error.contains(private_name));

        let in_repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benches/meeting_transcript_quality/private-manifest.template.json");
        let error = canonical_private_input(&in_repository, &repository)
            .expect_err("repository-local private manifest is rejected");
        assert!(!error.contains("private-manifest.template.json"));
    }

    #[test]
    fn native_timestamps_and_source_provenance_are_only_projected_for_scoring() {
        let sources = vec![
            AudioSource {
                id: "system-0".to_owned(),
                track: Track::System,
                wav_path: PathBuf::from("private-system.wav"),
                timeline_start_ms: 1_000,
            },
            AudioSource {
                id: "microphone-0".to_owned(),
                track: Track::Microphone,
                wav_path: PathBuf::from("private-microphone.wav"),
                timeline_start_ms: 1_500,
            },
        ];
        let transcripts = vec![
            TimedTranscript {
                text: "private system hypothesis".to_owned(),
                language: Some("en".to_owned()),
                duration_ms: 2_000,
                segments: vec![murmur_lib::stt::TimedSegment {
                    start_ms: 200,
                    end_ms: 800,
                    text: "private system hypothesis".to_owned(),
                    confidence: None,
                }],
            },
            TimedTranscript {
                text: "private microphone hypothesis".to_owned(),
                language: Some("en".to_owned()),
                duration_ms: 2_000,
                segments: vec![murmur_lib::stt::TimedSegment {
                    start_ms: 100,
                    end_ms: 400,
                    text: "private microphone hypothesis".to_owned(),
                    confidence: None,
                }],
            },
        ];
        let before = transcripts.clone();

        let projected = hypothesis_segments_from_native(&sources, &transcripts)
            .expect("native segments project onto the fixture timeline");

        assert_eq!(transcripts, before);
        assert_eq!(projected[0].track, Track::System);
        assert_eq!((projected[0].start_ms, projected[0].end_ms), (1_200, 1_800));
        assert_eq!(projected[1].track, Track::Microphone);
        assert_eq!((projected[1].start_ms, projected[1].end_ms), (1_600, 1_900));
    }

    #[test]
    fn source_activity_windows_reproduce_startup_and_trailing_candidate_shapes() {
        let path = temp_activity_wav("candidate-windows");
        write_activity_fixture(&path);
        let source = activity_source(path.clone());

        let first_startup = measure_activity_window(&path, &source, &activity_window(0, 7_000))
            .expect("first startup window measures");
        let broad_second = measure_activity_window(&path, &source, &activity_window(7_000, 32_000))
            .expect("second startup window measures");
        let supported_response =
            measure_activity_window(&path, &source, &activity_window(29_600, 30_000))
                .expect("bounded response window measures");
        let near_silence =
            measure_activity_window(&path, &source, &activity_window(32_000, 33_000))
                .expect("near-silence window measures");
        let digital_silence =
            measure_activity_window(&path, &source, &activity_window(33_000, 34_000))
                .expect("digital-silence window measures");

        assert_eq!((first_startup.peak, first_startup.rms), (0.0, 0.0));
        assert_eq!(broad_second.frame_count, 25_000);
        assert_eq!(broad_second.sample_count, 25_000);
        assert!((broad_second.peak - 16_384.0 / 32_767.0).abs() < 0.000_001);
        assert!(broad_second.rms > 0.0);
        assert!((supported_response.peak - broad_second.peak).abs() < f64::EPSILON);
        assert!((supported_response.rms - supported_response.peak).abs() < f64::EPSILON);
        assert!(broad_second.rms < supported_response.rms);
        assert!((near_silence.peak - 1.0 / 32_767.0).abs() < f64::EPSILON);
        assert_eq!(near_silence.peak, near_silence.rms);
        assert_eq!((digital_silence.peak, digital_silence.rms), (0.0, 0.0));

        std::fs::remove_file(path).expect("synthetic activity WAV is removed");
    }

    #[test]
    fn duplicate_and_punctuation_only_candidates_survive_native_projection_and_scoring() {
        let source = AudioSource {
            id: "source-a".to_owned(),
            track: Track::Microphone,
            wav_path: PathBuf::from("synthetic.wav"),
            timeline_start_ms: 0,
        };
        let transcript = TimedTranscript {
            text: "unused aggregate".to_owned(),
            language: Some("en".to_owned()),
            duration_ms: 34_000,
            segments: vec![
                murmur_lib::stt::TimedSegment {
                    start_ms: 0,
                    end_ms: 7_000,
                    text: "Alpha.".to_owned(),
                    confidence: None,
                },
                murmur_lib::stt::TimedSegment {
                    start_ms: 7_000,
                    end_ms: 32_000,
                    text: "Alpha.".to_owned(),
                    confidence: None,
                },
                murmur_lib::stt::TimedSegment {
                    start_ms: 32_000,
                    end_ms: 33_000,
                    text: ".".to_owned(),
                    confidence: None,
                },
                murmur_lib::stt::TimedSegment {
                    start_ms: 33_000,
                    end_ms: 34_000,
                    text: "...".to_owned(),
                    confidence: None,
                },
            ],
        };
        let before = transcript.clone();
        let hypotheses =
            hypothesis_segments_from_native(&[source], std::slice::from_ref(&transcript))
                .expect("native candidate segments project");
        assert_eq!(transcript, before);
        assert_eq!(hypotheses.len(), 4);
        assert_eq!(
            hypotheses
                .iter()
                .map(|segment| (segment.start_ms, segment.end_ms, segment.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (0, 7_000, "Alpha."),
                (7_000, 32_000, "Alpha."),
                (32_000, 33_000, "."),
                (33_000, 34_000, "..."),
            ]
        );

        let fixture = Fixture {
            version: FIXTURE_VERSION,
            fixture_id: "synthetic-startup-trailing-v1".to_owned(),
            reference_segments: vec![Segment {
                id: "reference-a".to_owned(),
                track: Track::Microphone,
                start_ms: 29_600,
                end_ms: 30_000,
                text: "Alpha.".to_owned(),
                kind: RegionKind::Acknowledgment,
            }],
            hypothesis_segments: hypotheses,
            key_terms: Vec::new(),
        };
        let report = score(&fixture).expect("synthetic candidate fixture scores");
        assert_eq!(report.hypothesis_segments, 4);
        assert_eq!(report.repeated_segments, 2);
    }

    #[test]
    fn activity_report_omits_source_ids_paths_text_and_classification() {
        let report = ActivityReport {
            windows: vec![ActivityWindowReport {
                track: Track::System,
                start_ms: 1_000,
                end_ms: 2_000,
                duration_ms: 1_000,
                frame_count: 16_000,
                sample_count: 32_000,
                peak: 0.25,
                rms: 0.05,
            }],
        };
        let serialized = serde_json::to_string(&report).expect("activity report serializes");

        for forbidden in [
            "synthetic-activity-v1",
            "source-a",
            "private-system.wav",
            "/Users/fixture-owner",
            "participant@example.test",
            "text",
            "silence",
            "speech",
        ] {
            assert!(!serialized.contains(forbidden));
        }
        for required in ["start_ms", "frame_count", "peak", "rms", "system"] {
            assert!(serialized.contains(required));
        }
    }

    #[test]
    fn activity_mode_writes_only_aggregate_numeric_evidence() {
        let root = std::env::temp_dir().join(format!(
            "murmur-meeting-activity-mode-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("private synthetic root is created");
        let wav = root.join("source.wav");
        write_activity_fixture(&wav);
        let manifest_path = root.join("activity.json");
        let output_path = root.join("aggregate.json");
        let manifest = serde_json::json!({
            "version": ACTIVITY_MANIFEST_VERSION,
            "corpus_id": "synthetic-activity-v1",
            "consent_confirmed": true,
            "audio_sources": [{
                "id": "source-private-id",
                "track": "microphone",
                "wav_path": "source.wav",
                "timeline_start_ms": 0
            }],
            "windows": [{
                "source_id": "source-private-id",
                "start_ms": 29_600,
                "end_ms": 30_000
            }]
        });
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("private synthetic manifest serializes"),
        )
        .expect("private synthetic manifest is written");

        run_activity_mode(&manifest_path, &output_path).expect("activity mode succeeds");

        let output = std::fs::read_to_string(&output_path).expect("aggregate report is readable");
        assert!(!output.contains("synthetic-activity-v1"));
        assert!(!output.contains("source-private-id"));
        assert!(!output.contains("source.wav"));
        assert!(output.contains("\"track\": \"microphone\""));
        assert!(output.contains("\"frame_count\": 400"));
        assert!(output.contains("\"peak\""));
        assert!(output.contains("\"rms\""));
        std::fs::remove_dir_all(root).expect("private synthetic root is removed");
    }

    #[test]
    fn activity_manifest_requires_consent_and_content_safe_metadata() {
        let mut manifest = ActivityManifest {
            version: ACTIVITY_MANIFEST_VERSION,
            corpus_id: "synthetic-activity-v1".to_owned(),
            consent_confirmed: false,
            audio_sources: vec![activity_source(PathBuf::from("private.wav"))],
            windows: vec![activity_window(0, 1_000)],
        };
        assert_eq!(
            validate_activity_manifest(&manifest).unwrap_err(),
            "explicit corpus-owner consent must be confirmed"
        );
        manifest.consent_confirmed = true;
        validate_activity_manifest(&manifest).expect("synthetic activity metadata validates");
        manifest.windows[0].source_id = "unknown-source".to_owned();
        assert_eq!(
            validate_activity_manifest(&manifest).unwrap_err(),
            "activity window metadata is invalid"
        );
    }

    #[test]
    fn value_bearing_cli_flags_reject_missing_values() {
        for flag in [
            "--input",
            "--manifest",
            "--activity-manifest",
            "--model",
            "--output",
        ] {
            assert_eq!(
                parse_args_from([flag.to_owned()]).err().as_deref(),
                Some("argument value is missing")
            );
        }
    }

    #[test]
    fn activity_output_cannot_replace_its_manifest_or_wav() {
        let root = std::env::temp_dir().join(format!(
            "murmur-meeting-activity-alias-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("private synthetic root is created");
        let wav = root.join("source.wav");
        write_activity_fixture(&wav);
        let manifest_path = root.join("activity.json");
        let manifest = serde_json::json!({
            "version": ACTIVITY_MANIFEST_VERSION,
            "corpus_id": "synthetic-activity-v1",
            "consent_confirmed": true,
            "audio_sources": [{
                "id": "source-private-id",
                "track": "microphone",
                "wav_path": "source.wav",
                "timeline_start_ms": 0
            }],
            "windows": [{
                "source_id": "source-private-id",
                "start_ms": 29_600,
                "end_ms": 30_000
            }]
        });
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("private synthetic manifest serializes"),
        )
        .expect("private synthetic manifest is written");
        let manifest_before = std::fs::read(&manifest_path).expect("manifest is readable");
        let wav_before = std::fs::read(&wav).expect("WAV is readable");
        let wav_alias = root.join("source-alias.wav");
        std::fs::hard_link(&wav, &wav_alias).expect("hard-link alias is created");

        assert_eq!(
            run_activity_mode(&manifest_path, &manifest_path).unwrap_err(),
            "aggregate report target must not already exist"
        );
        assert_eq!(
            run_activity_mode(&manifest_path, &wav).unwrap_err(),
            "aggregate report target must not already exist"
        );
        assert_eq!(
            run_activity_mode(&manifest_path, &wav_alias).unwrap_err(),
            "aggregate report target must not already exist"
        );
        assert_eq!(
            std::fs::read(&manifest_path).expect("manifest remains readable"),
            manifest_before
        );
        assert_eq!(
            std::fs::read(&wav).expect("WAV remains readable"),
            wav_before
        );
        std::fs::remove_dir_all(root).expect("private synthetic root is removed");
    }

    #[test]
    fn score_output_cannot_replace_its_input_or_a_hard_link_alias() {
        let root = std::env::temp_dir().join(format!(
            "murmur-meeting-score-alias-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("synthetic root is created");
        let input = root.join("fixture.json");
        std::fs::write(
            &input,
            include_bytes!("../benches/meeting_transcript_quality/synthetic-regression.json"),
        )
        .expect("synthetic fixture is copied");
        let alias = root.join("fixture-alias.json");
        std::fs::hard_link(&input, &alias).expect("hard-link alias is created");
        let before = std::fs::read(&input).expect("fixture is readable");

        assert_eq!(
            run_score_mode(&input, Some(&input)).unwrap_err(),
            "aggregate report could not be created"
        );
        assert_eq!(
            run_score_mode(&input, Some(&alias)).unwrap_err(),
            "aggregate report could not be created"
        );
        assert_eq!(
            std::fs::read(&input).expect("fixture remains readable"),
            before
        );
        std::fs::remove_dir_all(root).expect("synthetic root is removed");
    }

    #[cfg(unix)]
    #[test]
    fn dangling_output_symlink_cannot_escape_the_external_report_boundary() {
        use std::os::unix::fs::symlink;

        let repository = repository_root().expect("repository resolves");
        let root = std::env::temp_dir().join(format!(
            "murmur-meeting-output-symlink-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("synthetic root is created");
        let target = repository.join(format!(
            ".never-created-activity-report-{}",
            std::process::id()
        ));
        assert!(!target.exists());
        let output = root.join("aggregate.json");
        symlink(&target, &output).expect("dangling output symlink is created");

        assert_eq!(
            ensure_private_output(&output, &repository).unwrap_err(),
            "aggregate report target must not already exist"
        );
        assert!(!target.exists());
        std::fs::remove_dir_all(root).expect("synthetic root is removed");
    }

    #[test]
    fn gated_report_labels_the_promoted_production_policy() {
        assert!(MEETING_MICROPHONE_GATE_POLICY.contains("positive_frames=6"));
        assert!(MEETING_MICROPHONE_GATE_POLICY.contains("initial_context_ms=300"));
        assert!(MEETING_MICROPHONE_GATE_POLICY.contains("minimum_real_region_ms=1700"));
        assert!(!MEETING_MICROPHONE_GATE_POLICY.contains("positive_frames=5"));
    }

    #[test]
    fn aggregate_pipeline_report_contains_no_private_content_or_paths() {
        let mut report = score(&synthetic_fixture()).expect("fixture scores");
        report.pipeline = Some(PipelineMetrics {
            speech_gate: None,
            stt_model_id: DEFAULT_STT_MODEL_ID.to_owned(),
            shipping_model_sha256_verified: true,
            audio_sources: 2,
            microphone_audio_sources: 1,
            system_audio_sources: 1,
            total_audio_duration_ms: 4_000,
            inference_duration_ms: 900,
            process_peak_rss_bytes: Some(123_456),
        });
        let serialized = serde_json::to_string(&report).expect("report serializes");

        for forbidden in [
            "reference-context",
            "Let's take a break",
            "private-system.wav",
            "/Users/fixture-owner",
            "participant@example.test",
            "ggml-small.bin",
        ] {
            assert!(!serialized.contains(forbidden));
        }
        assert!(serialized.contains("inference_duration_ms"));
        assert!(serialized.contains("process_peak_rss_bytes"));
    }
}
