//! Opt-in, read-only, content-safe replay of an independently confirmed silent
//! meeting microphone. Never accesses Murmur's capture database or writes audio.
//! Input manifest and output report must stay outside the repository.
use murmur_lib::{
    model_download::{
        build_stt_engine_for, model_file_matches_registry_pin, ModelRegistry, DEFAULT_STT_MODEL_ID,
    },
    stt::{SpeechGateMode, SttOptions},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    consent_confirmed: bool,
    independently_confirmed_silent_microphone: bool,
    microphone_wavs: Vec<PathBuf>,
}

#[derive(Serialize)]
struct Report {
    gate: &'static str,
    units: usize,
    unit_duration_ms: u64,
    nonempty_units: usize,
    final_microphone_segments: usize,
    final_microphone_text_bytes: usize,
    processing_ms: u64,
}

fn path_outside_repo(path: &Path, repo: &Path) -> Result<PathBuf, &'static str> {
    let canonical = fs::canonicalize(path).map_err(|_| "input unavailable")?;
    if !canonical.is_file() || canonical.starts_with(repo) {
        return Err("input must be a regular file outside the repository");
    }
    Ok(canonical)
}

async fn run(manifest: PathBuf, model: PathBuf, output: PathBuf) -> Result<(), &'static str> {
    let repo = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("repository unavailable")?,
    )
    .map_err(|_| "repository unavailable")?;
    let manifest = path_outside_repo(&manifest, &repo)?;
    let model = path_outside_repo(&model, &repo)?;
    let parent = fs::canonicalize(output.parent().ok_or("output parent unavailable")?)
        .map_err(|_| "output parent unavailable")?;
    if !output.is_absolute() || parent.starts_with(&repo) || fs::symlink_metadata(&output).is_ok() {
        return Err("output must be new and outside the repository");
    }
    let input: Manifest =
        serde_json::from_slice(&fs::read(&manifest).map_err(|_| "manifest unreadable")?)
            .map_err(|_| "manifest invalid")?;
    if input.version != 1
        || !input.consent_confirmed
        || !input.independently_confirmed_silent_microphone
        || input.microphone_wavs.is_empty()
        || input.microphone_wavs.len() > 24
    {
        return Err("manifest consent or shape invalid");
    }
    let wavs: Vec<_> = input
        .microphone_wavs
        .iter()
        .map(|path| {
            path_outside_repo(path, &repo).and_then(|path| {
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                {
                    Ok(path)
                } else {
                    Err("input must be WAV")
                }
            })
        })
        .collect::<Result<_, _>>()?;
    if wavs
        .iter()
        .any(|wav| wav == &manifest || wav == &model || wav == &output)
    {
        return Err("input and output cannot alias");
    }
    let registry = ModelRegistry::get(DEFAULT_STT_MODEL_ID).ok_or("pinned model unavailable")?;
    if !model_file_matches_registry_pin(&model, registry)
        .map_err(|_| "pinned model verification failed")?
    {
        return Err("pinned model mismatch");
    }
    // Do not allow whisper.cpp native logs to print file paths or model text.
    whisper_rs::install_whisper_log_trampoline();
    let engine = build_stt_engine_for(registry, model, false);
    if engine.id() != "whisper" {
        return Err("unsupported model engine");
    }
    let options = SttOptions {
        speech_gate: SpeechGateMode::MeetingMicrophone,
        ..SttOptions::default()
    };
    let started = Instant::now();
    let mut report = Report {
        gate: "silero-v6-positive-frames-6-minimum-real-region-1700ms",
        units: 0,
        unit_duration_ms: 0,
        nonempty_units: 0,
        final_microphone_segments: 0,
        final_microphone_text_bytes: 0,
        processing_ms: 0,
    };
    for wav in &wavs {
        let decoded = engine.transcribe_timed(wav, &options).await;
        engine.unload().await;
        let decoded = decoded.map_err(|_| "meeting microphone transcription failed")?;
        report.units += 1;
        report.unit_duration_ms = report
            .unit_duration_ms
            .checked_add(u64::from(decoded.duration_ms))
            .ok_or("duration overflow")?;
        report.nonempty_units += usize::from(!decoded.text.is_empty());
        report.final_microphone_segments += decoded.segments.len();
        report.final_microphone_text_bytes += decoded.text.len();
    }
    report.processing_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let serialized = serde_json::to_vec_pretty(&report).map_err(|_| "report encoding failed")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|_| "report target unavailable")?;
    file.write_all(&serialized)
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|_| "report write failed")?;
    println!(
        "silent microphone replay complete: {} units; {} final segments; {} text bytes",
        report.units, report.final_microphone_segments, report.final_microphone_text_bytes
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: meeting_silent_microphone_replay <external-manifest> <pinned-model> <new-external-report>");
        std::process::exit(2);
    }
    if let Err(reason) = run(
        args[0].clone().into(),
        args[1].clone().into(),
        args[2].clone().into(),
    )
    .await
    {
        eprintln!("silent microphone replay failed: {reason}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_requires_confirmation_and_does_not_accept_unknown_private_metadata() {
        let payload = br#"{"version":1,"consent_confirmed":false,"independently_confirmed_silent_microphone":true,"microphone_wavs":[]}"#;
        let input: Manifest = serde_json::from_slice(payload).unwrap();
        assert!(!input.consent_confirmed);
        assert!(serde_json::from_slice::<Manifest>(br#"{"version":1,"consent_confirmed":true,"independently_confirmed_silent_microphone":true,"microphone_wavs":[],"transcript":"private"}"#).is_err());
    }
}
