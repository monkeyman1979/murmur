//! Per-unit Silero v6 inference. No process-global live session or audio state.
use ort::{session::Session, value::Tensor};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

use super::{check_cancelled, FRAME};
use crate::{
    error::{Error, Result},
    stt::SttOptions,
};

const MODEL: &[u8] = include_bytes!("../../../resources/silero-v6.onnx");
const MODEL_SHA256: &str = "597d30b3ec076608d059477bb14cfeffdf951bf5cae370d38f65d33bbfe82004";
// ort requires a process-global environment. Configure it lazily on the first
// gated microphone unit; no session, inference pool, or recurrent state is cached.
static PRIVATE_CPU_ENVIRONMENT: OnceLock<bool> = OnceLock::new();

fn native_error(category: &str) -> Error {
    Error::Backend(format!("meeting speech gate {category} failed"))
}

pub(crate) fn scan(pcm: &[f32], opts: &SttOptions) -> Result<Vec<f32>> {
    check_cancelled(opts.cancellation.as_ref())?;
    if format!("{:x}", Sha256::digest(MODEL)) != MODEL_SHA256 {
        return Err(native_error("model integrity"));
    }
    if !*PRIVATE_CPU_ENVIRONMENT.get_or_init(|| ort::init().with_telemetry(false).commit()) {
        return Err(native_error("private runtime configuration"));
    }
    // A fresh CPU-only session and recurrent state for each durable unit. No EP
    // registration: ONNX Runtime's default CPU execution provider handles all ops.
    let mut session = Session::builder()
        .map_err(|_| native_error("runtime"))?
        .with_intra_threads(1)
        .map_err(|_| native_error("runtime"))?
        .with_inter_threads(1)
        .map_err(|_| native_error("runtime"))?
        .commit_from_memory(MODEL)
        .map_err(|_| native_error("model load"))?;
    if session
        .inputs()
        .iter()
        .map(|v| v.name())
        .collect::<Vec<_>>()
        != ["input", "state", "sr"]
        || session
            .outputs()
            .iter()
            .map(|v| v.name())
            .collect::<Vec<_>>()
            != ["output", "stateN"]
    {
        return Err(native_error("tensor contract"));
    }
    let mut context = [0.0f32; 64];
    let mut state = vec![0.0f32; 256];
    let mut scores = Vec::with_capacity(pcm.len().div_ceil(FRAME));
    for chunk in pcm.chunks(FRAME) {
        check_cancelled(opts.cancellation.as_ref())?;
        let mut input = Vec::with_capacity(576);
        input.extend_from_slice(&context);
        input.extend_from_slice(chunk);
        input.resize(576, 0.0);
        let outputs = session.run(ort::inputs![
            "input" => Tensor::<f32>::from_array(([1, 576], input)).map_err(|_| native_error("tensor input"))?,
            "state" => Tensor::<f32>::from_array(([2, 1, 128], state)).map_err(|_| native_error("tensor input"))?,
            "sr" => Tensor::<i64>::from_array(((), vec![16000])).map_err(|_| native_error("tensor input"))?
        ]).map_err(|_| native_error("inference"))?;
        let (shape, values) = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(|_| native_error("tensor output"))?;
        if shape.as_ref() != [1, 1]
            || values.len() != 1
            || !values[0].is_finite()
            || !(0.0..=1.0).contains(&values[0])
        {
            return Err(native_error("score"));
        }
        scores.push(values[0]);
        let (shape, values) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|_| native_error("tensor state"))?;
        if shape.as_ref() != [2, 1, 128]
            || values.len() != 256
            || values.iter().any(|v| !v.is_finite())
        {
            return Err(native_error("tensor state"));
        }
        state = values.to_vec();
        // Upstream uses the final 64 samples of the *current new frame*, not
        // the previous 64-sample context. Zero-fill only the last partial frame.
        context.copy_from_slice(&chunk_frame_tail(chunk));
    }
    check_cancelled(opts.cancellation.as_ref())?;
    Ok(scores)
}

fn chunk_frame_tail(chunk: &[f32]) -> [f32; 64] {
    let mut frame = [0.0f32; FRAME];
    frame[..chunk.len()].copy_from_slice(chunk);
    frame[FRAME - 64..]
        .try_into()
        .expect("fixed context length")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_frame_context_matches_zero_padded_input() {
        assert_eq!(chunk_frame_tail(&[0.5; 3]), [0.0; 64]);
        assert_eq!(chunk_frame_tail(&[0.5; FRAME]), [0.5; 64]);
    }

    #[test]
    fn verified_embedded_model_runs_cpu_on_zero_pcm() {
        let scores = scan(&vec![0.0; FRAME * 2], &SttOptions::default()).unwrap();
        assert_eq!(scores.len(), 2);
        assert!((scores[0] - 0.023828626).abs() < 0.001);
        assert!(scores.iter().all(|score| score.is_finite()));
    }
}
