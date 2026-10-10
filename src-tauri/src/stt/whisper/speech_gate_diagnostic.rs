//! Opt-in public stage diagnosis and frozen-candidate validation.
//! No production logging or policy override.
//! Generated text stays in memory. Reports contain generated IDs and numeric/categorical
//! evidence only. The ordinary benchmark remains authoritative for historical scoring.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;

mod validation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    corpus_id: String,
    consent_confirmed: bool,
    language: Option<String>,
    initial_prompt: Option<String>,
    audio_sources: Vec<Source>,
    reference_segments: Vec<Reference>,
    key_terms: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    track: String,
    wav_path: PathBuf,
    timeline_start_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    track: String,
    start_ms: u64,
    end_ms: u64,
    text: String,
    kind: String,
}
#[derive(Serialize)]
struct Recognition {
    segments: usize,
    tokens: usize,
    token_errors: usize,
    lexical_match: bool,
    timed_match: bool,
    // Numeric region/unit-relative times, never generated words.
    segment_bounds_ms: Vec<[u32; 2]>,
}
#[derive(Serialize)]
struct RegionEvidence {
    bounds_samples: [usize; 2],
    real_samples: usize,
    decoder_samples: usize,
    normalized_native_segments: usize,
    padding_only_drops: usize,
    end_clamps: usize,
    mapped_segments: usize,
    native_recognition: Recognition,
    detected_language_equals_baseline: bool,
}
#[derive(Serialize)]
struct SourceEvidence {
    source_id: String,
    reference_id: String,
    kind: String,
    canonical_samples: usize,
    original_samples: usize,
    onset_positive_frames: usize,
    core_bounds_samples: Vec<[usize; 2]>,
    omitted_original_samples: usize,
    omitted_nonzero_original_samples: usize,
    baseline: Recognition,
    gated: Recognition,
    detected_language_equals_baseline: bool,
    regions: Vec<RegionEvidence>,
}

// Exact normalization used by the benchmark. For these isolated ONE-WORD
// references its >=0.6 token-recall rule reduces to token presence; ten-second
// spacing prevents cross-source matches. This does not replace its general scorer.
fn tokens(text: &str) -> Vec<String> {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}
fn recognition(segments: &[TimedSegment], reference: &str, reference_end_ms: u32) -> Recognition {
    let hypothesis: Vec<_> = segments.iter().flat_map(|s| tokens(&s.text)).collect();
    let lexical_match = hypothesis.iter().any(|t| t == reference);
    let timed_match = segments.iter().any(|s| {
        s.start_ms <= reference_end_ms.saturating_add(2_000)
            && tokens(&s.text).iter().any(|t| t == reference)
    });
    // Levenshtein distance from exactly one reference token.
    let token_errors = if hypothesis.is_empty() {
        1
    } else {
        hypothesis.len() - usize::from(lexical_match)
    };
    Recognition {
        segments: segments.len(),
        tokens: hypothesis.len(),
        token_errors,
        lexical_match,
        timed_match,
        segment_bounds_ms: segments.iter().map(|s| [s.start_ms, s.end_ms]).collect(),
    }
}
fn external_file(path: &Path, repo: &Path) -> PathBuf {
    let path = fs::canonicalize(path).expect("diagnostic input unavailable");
    assert!(
        path.is_file() && !path.starts_with(repo),
        "input must be external"
    );
    path
}
fn validate_manifest(m: &Manifest) {
    assert!(
        m.version == 1 && m.consent_confirmed,
        "explicit consent required"
    );
    assert!(m.language.is_none() && m.initial_prompt.is_none() && m.key_terms.is_empty());
    assert!(!m.corpus_id.is_empty());
    assert!(m.audio_sources.len() == 14 && m.reference_segments.len() == 14);
    for (i, (s, r)) in m
        .audio_sources
        .iter()
        .zip(&m.reference_segments)
        .enumerate()
    {
        assert!(!s.id.is_empty() && !r.id.is_empty());
        assert!(s.track == "microphone" && r.track == "microphone");
        assert!(s.timeline_start_ms == i as u64 * 10_000 && r.start_ms == s.timeline_start_ms);
        assert!(r.end_ms > r.start_ms && r.end_ms - r.start_ms <= 1_000);
        assert!(tokens(&r.text).len() == 1);
        assert!(r.kind == "acknowledgment" || r.kind == "short_turn");
    }
}

#[tokio::test]
#[ignore = "requires approved external 14-clip padded manifest, pinned model and new report"]
async fn approved_public_five_frame_stage_diagnostic() {
    run_public_stage_diagnostic(false).await;
}

#[tokio::test]
#[ignore = "configuration slot 3; requires approved public control, pinned model and new report"]
async fn approved_public_final_candidate_stage_diagnostic() {
    run_public_stage_diagnostic(true).await;
}

async fn run_public_stage_diagnostic(final_candidate: bool) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let manifest_path = external_file(
        &PathBuf::from(
            std::env::var_os("MURMUR_APPROVED_PUBLIC_SPEECH_MANIFEST").expect("manifest required"),
        ),
        &repo,
    );
    let output = PathBuf::from(
        std::env::var_os("MURMUR_GATE_DIAGNOSTIC_REPORT").expect("new report required"),
    );
    assert!(output.is_absolute());
    let parent = output
        .parent()
        .unwrap()
        .canonicalize()
        .expect("external report parent required");
    assert!(!parent.starts_with(&repo));
    // create_new also refuses dangling symlinks and existing input/report aliases.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .expect("new report required");
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(manifest_path).expect("manifest unreadable"))
            .expect("manifest shape invalid");
    validate_manifest(&manifest);
    let model_path = external_file(
        &PathBuf::from(
            std::env::var_os("MURMUR_WHISPER_MODEL_PATH").expect("pinned model required"),
        ),
        &repo,
    );
    let registry =
        crate::model_download::ModelRegistry::get(crate::model_download::DEFAULT_STT_MODEL_ID)
            .unwrap();
    assert!(crate::model_download::model_file_matches_registry_pin(&model_path, registry).unwrap());
    whisper_rs::install_whisper_log_trampoline();
    let engine = WhisperEngine::new(
        model_path,
        Some((registry.expected_bytes, registry.sha256.unwrap())),
    );
    let off = SttOptions::default();
    let on = SttOptions {
        speech_gate: SpeechGateMode::MeetingMicrophone,
        ..off.clone()
    };
    let mut reports = Vec::new();
    for (i, (source, reference)) in manifest
        .audio_sources
        .iter()
        .zip(&manifest.reference_segments)
        .enumerate()
    {
        let path = external_file(&source.wav_path, &repo);
        let pcm = read_mono_16k_from_wav(&path)
            .map_err(|_| "canonical conversion failed")
            .unwrap();
        let original_samples = pcm
            .len()
            .checked_sub(11_200)
            .expect("700 ms padding required");
        assert!(pcm[original_samples..].iter().all(|v| *v == 0.0));
        assert!(
            original_samples * 1000 / speech_gate::RATE
                == (reference.end_ms - reference.start_ms) as usize
        );
        let reference_token = &tokens(&reference.text)[0];
        let reference_end = (reference.end_ms - reference.start_ms) as u32;
        let baseline = engine
            .transcribe_timed(&path, &off)
            .await
            .map_err(|_| "baseline decode failed")
            .unwrap();
        engine.unload().await;
        let scores = speech_gate::scan(&pcm, &on)
            .map_err(|_| "detector failed")
            .unwrap();
        let (cores, regions) =
            speech_gate::diagnostic_selection(&scores, pcm.len(), final_candidate).unwrap();
        if !final_candidate {
            assert!(regions == speech_gate::regions(&scores, pcm.len()).unwrap());
        }
        let omitted: Vec<_> = (0..original_samples)
            .filter(|sample| {
                !regions
                    .iter()
                    .any(|r| r.start <= *sample && *sample < r.end)
            })
            .collect();
        let mut region_reports = Vec::new();
        let ctx = (!regions.is_empty()).then(|| {
            engine
                .load_context()
                .map_err(|_| "model load failed")
                .unwrap()
        });
        let mut region_index = 0;
        let gated = speech_gate::decode_regions(&pcm, &regions, &on, |input| {
            let region = regions[region_index];
            region_index += 1;
            let decoder_samples = input.len();
            let decoded = transcribe_blocking(Arc::clone(ctx.as_ref().unwrap()), input, &on)?;
            let real_ms = ((region.end - region.start) * 1000 / speech_gate::RATE) as u32;
            let drops = decoded
                .segments
                .iter()
                .filter(|s| s.start_ms >= real_ms)
                .count();
            let clamps = decoded
                .segments
                .iter()
                .filter(|s| s.start_ms < real_ms && s.end_ms > real_ms)
                .count();
            region_reports.push(RegionEvidence {
                bounds_samples: [region.start, region.end],
                real_samples: region.end - region.start,
                decoder_samples,
                normalized_native_segments: decoded.segments.len(),
                padding_only_drops: drops,
                end_clamps: clamps,
                mapped_segments: decoded.segments.len() - drops,
                native_recognition: recognition(&decoded.segments, reference_token, reference_end),
                detected_language_equals_baseline: decoded.language == baseline.language,
            });
            Ok(TimedTranscript {
                text: decoded.text,
                language: decoded.language,
                duration_ms: (decoder_samples * 1000 / speech_gate::RATE) as u32,
                segments: decoded.segments,
            })
        })
        .map_err(|_| "gated decode failed")
        .unwrap();
        drop(ctx);
        engine.unload().await;
        reports.push(SourceEvidence {
            source_id: format!("source-{:02}", i + 1),
            reference_id: format!("reference-{:02}", i + 1),
            kind: reference.kind.clone(),
            canonical_samples: pcm.len(),
            original_samples,
            onset_positive_frames: scores.iter().filter(|s| **s >= 0.5).count(),
            core_bounds_samples: cores.iter().map(|r| [r.start, r.end]).collect(),
            omitted_original_samples: omitted.len(),
            omitted_nonzero_original_samples: omitted.iter().filter(|s| pcm[**s] != 0.0).count(),
            baseline: recognition(&baseline.segments, reference_token, reference_end),
            gated: recognition(&gated.segments, reference_token, reference_end),
            detected_language_equals_baseline: gated.language == baseline.language,
            regions: region_reports,
        });
    }
    serde_json::to_writer_pretty(&mut file, &reports).unwrap();
    file.write_all(b"\n").unwrap();
    println!(
        "Public stage diagnosis complete: {} sources; numeric/categorical external report only",
        reports.len()
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SilentManifest {
    version: u32,
    consent_confirmed: bool,
    independently_confirmed_silent_microphone: bool,
    microphone_wavs: Vec<PathBuf>,
}

#[tokio::test]
#[ignore = "configuration slot 3; requires renewed full-silence consent, pinned model and new report"]
async fn approved_silent_final_candidate_replay() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let manifest_path = external_file(
        &PathBuf::from(
            std::env::var_os("MURMUR_APPROVED_SILENT_MANIFEST").expect("silent manifest required"),
        ),
        &repo,
    );
    let manifest: SilentManifest =
        serde_json::from_slice(&fs::read(manifest_path).expect("manifest unreadable"))
            .expect("manifest shape invalid");
    assert!(
        manifest.version == 1
            && manifest.consent_confirmed
            && manifest.independently_confirmed_silent_microphone
    );
    assert!(!manifest.microphone_wavs.is_empty() && manifest.microphone_wavs.len() <= 24);
    let output = PathBuf::from(
        std::env::var_os("MURMUR_GATE_DIAGNOSTIC_REPORT").expect("new report required"),
    );
    assert!(
        output.is_absolute()
            && !output
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .starts_with(&repo)
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .expect("new report required");
    let model = external_file(
        &PathBuf::from(
            std::env::var_os("MURMUR_WHISPER_MODEL_PATH").expect("pinned model required"),
        ),
        &repo,
    );
    let registry =
        crate::model_download::ModelRegistry::get(crate::model_download::DEFAULT_STT_MODEL_ID)
            .unwrap();
    assert!(crate::model_download::model_file_matches_registry_pin(&model, registry).unwrap());
    whisper_rs::install_whisper_log_trampoline();
    let engine = WhisperEngine::new(
        model,
        Some((registry.expected_bytes, registry.sha256.unwrap())),
    );
    let opts = SttOptions {
        speech_gate: SpeechGateMode::MeetingMicrophone,
        ..SttOptions::default()
    };
    let started = std::time::Instant::now();
    let mut rows = Vec::new();
    let mut duration_ms = 0_u64;
    let mut final_segments = 0;
    let mut final_bytes = 0;
    for (i, path) in manifest.microphone_wavs.iter().enumerate() {
        let path = external_file(path, &repo);
        let pcm = read_mono_16k_from_wav(&path)
            .map_err(|_| "canonical conversion failed")
            .unwrap();
        assert!(
            !pcm.is_empty()
                && pcm.len() <= speech_gate::RATE * 360
                && pcm.iter().all(|x| x.is_finite())
        );
        let scores = speech_gate::scan(&pcm, &opts)
            .map_err(|_| "detector failed")
            .unwrap();
        let (cores, regions) = speech_gate::diagnostic_selection(&scores, pcm.len(), true).unwrap();
        let ctx = (!regions.is_empty()).then(|| {
            engine
                .load_context()
                .map_err(|_| "model load failed")
                .unwrap()
        });
        let mut decoder_calls = 0;
        let decoded = speech_gate::decode_regions(&pcm, &regions, &opts, |input| {
            decoder_calls += 1;
            let duration_ms = (input.len() * 1000 / speech_gate::RATE) as u32;
            let result = transcribe_blocking(Arc::clone(ctx.as_ref().unwrap()), input, &opts)?;
            Ok(TimedTranscript {
                text: result.text,
                language: result.language,
                duration_ms,
                segments: result.segments,
            })
        })
        .map_err(|_| "gated decode failed")
        .unwrap();
        drop(ctx);
        engine.unload().await;
        duration_ms += u64::from(decoded.duration_ms);
        final_segments += decoded.segments.len();
        final_bytes += decoded.text.len();
        rows.push(
            serde_json::json!({"source_index":i+1,"duration_ms":decoded.duration_ms,
            "onset_positive_frames":scores.iter().filter(|s| **s>=0.5).count(),
            "cores":cores.len(),"regions":regions.len(),"decoder_calls":decoder_calls,
            "final_segments":decoded.segments.len(),"final_text_bytes":decoded.text.len()}),
        );
    }
    let report = serde_json::json!({"configuration":3,"positive_frames":6,"minimum_real_context_ms":1700,
        "units":rows.len(),"unit_duration_ms":duration_ms,"final_microphone_segments":final_segments,
        "final_microphone_text_bytes":final_bytes,"processing_ms":started.elapsed().as_millis(),"sources":rows});
    serde_json::to_writer_pretty(&mut file, &report).unwrap();
    file.write_all(b"\n").unwrap();
    println!("Complete silent candidate replay finished; numeric external report only");
}

#[test]
fn one_word_matching_distinguishes_substitution_from_timestamp_miss() {
    let segment = |text: &str, start_ms| TimedSegment {
        text: text.into(),
        start_ms,
        end_ms: start_ms + 100,
        confidence: None,
    };
    let substitute = recognition(&[segment("other", 0)], "test", 700);
    assert!(substitute.token_errors == 1 && !substitute.lexical_match && !substitute.timed_match);
    let late = recognition(&[segment("Test!", 3000)], "test", 700);
    assert!(late.token_errors == 0 && late.lexical_match && !late.timed_match);
    let matching = recognition(&[segment("TEST.", 0)], "test", 700);
    assert!(matching.token_errors == 0 && matching.lexical_match && matching.timed_match);
}
