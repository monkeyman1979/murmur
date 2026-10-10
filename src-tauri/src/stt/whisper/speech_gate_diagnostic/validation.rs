//! Fixed configuration-3 validation on independently referenced public speech.
//! No tunable policy, no text output, and stop on the first new word-level loss.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationManifest {
    version: u32,
    consent_confirmed: bool,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    wav_path: PathBuf,
    category: Category,
    reference_text: String,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Category {
    LongSpeech,
    LowLevelReadSpeech,
    BoundaryStress,
}
#[derive(Serialize)]
struct WordScore {
    reference_words: usize,
    hypothesis_words: usize,
    errors: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
    // Source-local reference positions, never tokens. Alignment ties are resolved
    // deterministically: exact diagonal, substitution, deletion, insertion.
    correct_reference_positions: Vec<usize>,
}
fn word_score(reference: &[String], hypothesis: &[String]) -> WordScore {
    let (n, m) = (reference.len(), hypothesis.len());
    assert!(
        n <= 2048 && m <= 4096,
        "diagnostic alignment bound exceeded"
    );
    let mut costs = vec![vec![0; m + 1]; n + 1];
    for (i, row) in costs.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in costs[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            costs[i][j] = (costs[i - 1][j - 1]
                + usize::from(reference[i - 1] != hypothesis[j - 1]))
            .min(costs[i - 1][j] + 1)
            .min(costs[i][j - 1] + 1);
        }
    }
    let mut score = WordScore {
        reference_words: n,
        hypothesis_words: m,
        errors: costs[n][m],
        substitutions: 0,
        deletions: 0,
        insertions: 0,
        correct_reference_positions: Vec::new(),
    };
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0
            && j > 0
            && reference[i - 1] == hypothesis[j - 1]
            && costs[i][j] == costs[i - 1][j - 1]
        {
            score.correct_reference_positions.push(i - 1);
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && costs[i][j] == costs[i - 1][j - 1] + 1 {
            score.substitutions += 1;
            i -= 1;
            j -= 1;
        } else if i > 0 && costs[i][j] == costs[i - 1][j] + 1 {
            score.deletions += 1;
            i -= 1;
        } else {
            assert!(j > 0);
            score.insertions += 1;
            j -= 1;
        }
    }
    score.correct_reference_positions.reverse();
    score
}

#[derive(Serialize)]
struct PairReport {
    duration_ms: u32,
    onset_positive_frames: usize,
    core_bounds_samples: Vec<[usize; 2]>,
    region_bounds_samples: Vec<[usize; 2]>,
    retained_real_samples: usize,
    decoder_samples: usize,
    baseline_segments: usize,
    native_gated_segments: usize,
    mapped_gated_segments: usize,
    padding_only_drops: usize,
    end_clamps: usize,
    detected_language_equals_baseline: bool,
    baseline: WordScore,
    native_gated: WordScore,
    gated: WordScore,
    newly_incorrect_reference_positions: Vec<usize>,
    regression: bool,
}
struct PairEvidence {
    report: PairReport,
    baseline_tokens: Vec<String>,
    gated_tokens: Vec<String>,
}

async fn run_pair(engine: &WhisperEngine, path: &Path, reference: &[String]) -> PairEvidence {
    let off = SttOptions::default();
    let on = SttOptions {
        speech_gate: SpeechGateMode::MeetingMicrophone,
        ..off.clone()
    };
    let baseline = engine
        .transcribe_timed(path, &off)
        .await
        .map_err(|_| "baseline decode failed")
        .unwrap();
    engine.unload().await;
    let pcm = read_mono_16k_from_wav(path)
        .map_err(|_| "canonical conversion failed")
        .unwrap();
    assert!(
        !pcm.is_empty()
            && pcm.len() <= 360 * speech_gate::RATE
            && pcm.iter().all(|v| v.is_finite())
    );
    let scores = speech_gate::scan(&pcm, &on)
        .map_err(|_| "detector failed")
        .unwrap();
    // Frozen configuration 3; never inferred from environment or user-supplied knobs.
    let (cores, regions) = speech_gate::diagnostic_selection(&scores, pcm.len(), true).unwrap();
    let ctx = (!regions.is_empty()).then(|| {
        engine
            .load_context()
            .map_err(|_| "model load failed")
            .unwrap()
    });
    let mut index = 0;
    let mut native_gated_segments = 0;
    let mut padding_only_drops = 0;
    let mut end_clamps = 0;
    let mut decoder_samples = 0;
    let mut native_tokens = Vec::new();
    let gated = speech_gate::decode_regions(&pcm, &regions, &on, |input| {
        let real_ms =
            ((regions[index].end - regions[index].start) * 1000 / speech_gate::RATE) as u32;
        index += 1;
        let duration_ms = (input.len() * 1000 / speech_gate::RATE) as u32;
        decoder_samples += input.len();
        let decoded = transcribe_blocking(Arc::clone(ctx.as_ref().unwrap()), input, &on)?;
        native_gated_segments += decoded.segments.len();
        native_tokens.extend(decoded.segments.iter().flat_map(|s| tokens(&s.text)));
        padding_only_drops += decoded
            .segments
            .iter()
            .filter(|s| s.start_ms >= real_ms)
            .count();
        end_clamps += decoded
            .segments
            .iter()
            .filter(|s| s.start_ms < real_ms && s.end_ms > real_ms)
            .count();
        Ok(TimedTranscript {
            text: decoded.text,
            language: decoded.language,
            duration_ms,
            segments: decoded.segments,
        })
    })
    .map_err(|_| "gated decode failed")
    .unwrap();
    drop(ctx);
    engine.unload().await;
    let baseline_tokens: Vec<_> = baseline
        .segments
        .iter()
        .flat_map(|s| tokens(&s.text))
        .collect();
    let gated_tokens: Vec<_> = gated
        .segments
        .iter()
        .flat_map(|s| tokens(&s.text))
        .collect();
    let base_score = word_score(reference, &baseline_tokens);
    let gate_score = word_score(reference, &gated_tokens);
    let newly_incorrect_reference_positions: Vec<_> = base_score
        .correct_reference_positions
        .iter()
        .filter(|i| !gate_score.correct_reference_positions.contains(i))
        .copied()
        .collect();
    // Conservative stop: alignment ties can require adjudication, but are never
    // used to excuse a measured word regression or to continue tuning silently.
    let regression =
        gate_score.errors > base_score.errors || !newly_incorrect_reference_positions.is_empty();
    PairEvidence {
        report: PairReport {
            duration_ms: baseline.duration_ms,
            onset_positive_frames: scores.iter().filter(|s| **s >= 0.5).count(),
            core_bounds_samples: cores.iter().map(|r| [r.start, r.end]).collect(),
            region_bounds_samples: regions.iter().map(|r| [r.start, r.end]).collect(),
            retained_real_samples: regions.iter().map(|r| r.end - r.start).sum(),
            decoder_samples,
            baseline_segments: baseline.segments.len(),
            native_gated_segments,
            mapped_gated_segments: gated.segments.len(),
            padding_only_drops,
            end_clamps,
            detected_language_equals_baseline: baseline.language == gated.language,
            baseline: base_score,
            native_gated: word_score(reference, &native_tokens),
            gated: gate_score,
            newly_incorrect_reference_positions,
            regression,
        },
        baseline_tokens,
        gated_tokens,
    }
}

#[tokio::test]
#[ignore = "frozen configuration 3; requires approved public references, pinned model and new report"]
async fn approved_fixed_candidate_speech_validation() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let manifest_path = external_file(
        &PathBuf::from(
            std::env::var_os("MURMUR_APPROVED_VALIDATION_MANIFEST").expect("manifest required"),
        ),
        &repo,
    );
    let manifest: ValidationManifest =
        serde_json::from_slice(&fs::read(manifest_path).expect("manifest unreadable"))
            .expect("invalid manifest shape");
    assert!(
        manifest.version == 1
            && manifest.consent_confirmed
            && !manifest.cases.is_empty()
            && manifest.cases.len() <= 16
    );
    // Validate the complete input list before any inference; no dynamic clip selection.
    let mut paths = Vec::new();
    let mut references = Vec::new();
    for case in &manifest.cases {
        let reference = tokens(&case.reference_text);
        assert!(!reference.is_empty() && reference.len() <= 2048);
        references.push(reference);
        let path = external_file(&case.wav_path, &repo);
        assert!(path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("wav")));
        paths.push(path);
    }
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
    let mut rows = Vec::new();
    let mut stopped = false;
    for (i, ((case, path), reference)) in manifest
        .cases
        .iter()
        .zip(&paths)
        .zip(&references)
        .enumerate()
    {
        let first = run_pair(&engine, path, reference).await;
        // Only an identical repeat is allowed, to distinguish native variability.
        // No other cases run after a first regression, even if the repeat differs.
        let repeat = if first.report.regression {
            Some(run_pair(&engine, path, reference).await)
        } else {
            None
        };
        let stability = repeat.as_ref().map(|r| serde_json::json!({
            "baseline_tokens_identical":r.baseline_tokens==first.baseline_tokens,
            "gated_tokens_identical":r.gated_tokens==first.gated_tokens,
            "core_bounds_identical":r.report.core_bounds_samples==first.report.core_bounds_samples,
            "region_bounds_identical":r.report.region_bounds_samples==first.report.region_bounds_samples,
        }));
        stopped = first.report.regression;
        rows.push(
            serde_json::json!({"source_index":i+1,"category":case.category,"first":first.report,
            "identical_repeat":repeat.as_ref().map(|r| &r.report),"repeat_stability":stability}),
        );
        if stopped {
            break;
        }
    }
    let report = serde_json::json!({"version":1,"configuration":3,"positive_frames":6,
        "minimum_available_real_context_ms":1700,"decoder_floor_ms":1100,
        "sources_preselected":manifest.cases.len(),"sources_run":rows.len(),
        "stopped_on_regression":stopped,"sources":rows});
    serde_json::to_writer_pretty(&mut file, &report).unwrap();
    file.write_all(b"\n").unwrap();
    println!("Fixed-candidate speech validation complete: {} sources; regression stop={stopped}; content-safe external report",rows.len());
}

#[test]
fn equal_word_error_totals_can_hide_new_reference_losses() {
    let reference = tokens("alpha beta gamma delta");
    let baseline = word_score(&reference, &tokens("alpha other gamma delta"));
    let gated = word_score(&reference, &tokens("alpha beta other delta"));
    assert_eq!(baseline.errors, gated.errors);
    let newly_incorrect: Vec<_> = baseline
        .correct_reference_positions
        .iter()
        .filter(|i| !gated.correct_reference_positions.contains(i))
        .copied()
        .collect();
    assert_eq!(newly_incorrect, vec![2]);
}

#[test]
fn alignment_reports_substitutions_deletions_insertions_without_text() {
    let reference = tokens("alpha beta gamma delta");
    let score = word_score(&reference, &tokens("alpha replacement delta extra"));
    assert_eq!(score.errors, 3);
    assert_eq!(
        score.errors,
        score.substitutions + score.deletions + score.insertions
    );
    let missing = word_score(&reference, &tokens("alpha gamma delta"));
    assert_eq!(missing.deletions, 1);
    assert_eq!(missing.correct_reference_positions, vec![0, 2, 3]);
    let repeated = word_score(&tokens("yes yes"), &tokens("yes"));
    assert_eq!(repeated.deletions, 1);
    assert_eq!(word_score(&reference, &reference).errors, 0);
    let serialized = serde_json::to_string(&score).unwrap();
    for text in ["alpha", "beta", "gamma", "delta", "replacement", "extra"] {
        assert!(!serialized.contains(text));
    }
}
