"""OpenAI-compatible speech gateway for the open-atc engine.

Serves faster-whisper STT and kokoro-onnx TTS behind the two OpenAI
endpoints the engine calls (engine appends these paths to the base URL
from settings):

  POST /v1/audio/transcriptions  multipart: file=<wav>, model=<ignored>
  POST /v1/audio/speech          json: {model, voice, input, response_format, speed,
                                  sentence_pause, clause_pause, effects:{hiss,
                                  crackle, static, bandpass}}
  GET  /health
  GET  /v1/voices                sorted kokoro voice names

Deploy: copy to the services host next to kokoro-v1.0.onnx + voices-v1.0.bin
and run from that directory (see scripts/atc-speech-gateway.service).
Install deps from scripts/speech-gateway-requirements.txt.

  speech_gateway.py [--host 127.0.0.1] [--port 8099]

GPU note: kokoro uses CUDA when ONNX_PROVIDER=CUDAExecutionProvider and the
nvidia pip runtime libs are on LD_LIBRARY_PATH (both set in the service unit).
Whisper uses CUDA when ctranslate2 sees the GPU, else CPU int8 (logged).
"""

import argparse
import hashlib
import io
import logging
import tempfile
import threading
from contextlib import asynccontextmanager

import numpy as np
import soundfile as sf
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import Response
from faster_whisper import WhisperModel
from kokoro_onnx import Kokoro

log = logging.getLogger("speech_gateway")

# OpenAI voice name -> kokoro voice (all exist in voices-v1.0.bin).
VOICE_MAP = {
    "alloy": "af_alloy",
    "echo": "am_echo",
    "fable": "bm_fable",
    "onyx": "am_onyx",
    "nova": "af_nova",
    "shimmer": "af_sky",
}
DEFAULT_VOICE = "af_bella"  # voice proven in local tests
MAX_TEXT = 4000  # matches engine-side limit

_lock = threading.Lock()
_tts = None
_stt = None
_status = {"tts_provider": "unknown", "stt_device": "unknown", "stt_model": "tiny"}


def _tts_providers(tts) -> str:
    session = getattr(tts, "sess", None) or getattr(tts, "session", None)
    if session is not None:
        try:
            return "+".join(session.get_providers())
        except Exception:
            pass
    import onnxruntime as rt

    return "+".join(rt.get_available_providers())


def _resolve_voice(name: str) -> str:
    if not name:
        return DEFAULT_VOICE
    if name in VOICE_MAP:
        return VOICE_MAP[name]
    try:
        if name in _tts.get_voices():
            return name
    except Exception:
        pass
    log.warning("Unknown voice %r, using %s", name, DEFAULT_VOICE)
    return DEFAULT_VOICE


@asynccontextmanager
async def _lifespan(app: FastAPI):
    global _tts, _stt
    _tts = Kokoro("kokoro-v1.0.onnx", "voices-v1.0.bin")
    _status["tts_provider"] = _tts_providers(_tts)
    log.info("TTS loaded, providers=%s", _status["tts_provider"])
    try:
        _stt = WhisperModel("tiny", device="cuda", compute_type="float16")
        _status["stt_device"] = "cuda/float16"
    except Exception as exc:
        log.warning("CUDA whisper unavailable (%s), using CPU int8", exc)
        _stt = WhisperModel("tiny", device="cpu", compute_type="int8")
        _status["stt_device"] = "cpu/int8"
    log.info("STT loaded, device=%s", _status["stt_device"])
    yield


app = FastAPI(title="open-atc speech gateway", lifespan=_lifespan)


@app.get("/health")
def health():
    return {"status": "ok", **_status}


@app.get("/v1/voices")
def voices():
    try:
        return {"voices": sorted(_tts.get_voices())}
    except Exception as exc:
        raise HTTPException(500, f"Cannot list voices: {exc}")


def _radio_effects(samples: np.ndarray, rate: int, fx: dict, seed_key: str) -> np.ndarray:
    """ATC radio chain: bandpass, hiss bed, crackle impulses, static bursts.

    Seeded from the message so identical requests render byte-identical audio.
    All levels are 0..1 fractions; no-ops (all zero, no bandpass) return input.
    """
    hiss = min(max(float(fx.get("hiss", 0)), 0.0), 1.0)
    crackle = min(max(float(fx.get("crackle", 0)), 0.0), 1.0)
    static = min(max(float(fx.get("static", 0)), 0.0), 1.0)
    bandpass = bool(fx.get("bandpass", False))
    if hiss <= 0 and crackle <= 0 and static <= 0 and not bandpass:
        return samples
    seed = int.from_bytes(hashlib.sha256(seed_key.encode()).digest()[:8], "little")
    rng = np.random.default_rng(seed)
    x = samples.astype(np.float64)
    if bandpass and rate >= 8000:
        spectrum = np.fft.rfft(x)
        freqs = np.fft.rfftfreq(len(x), 1.0 / rate)
        low = np.clip((freqs - 300.0 + 150.0) / 300.0, 0.0, 1.0)
        high = np.clip((3400.0 + 400.0 - freqs) / 800.0, 0.0, 1.0)
        x = np.fft.irfft(spectrum * low * high, len(x))
    peak = float(np.max(np.abs(x))) + 1e-9
    rms = float(np.sqrt(np.mean(x * x))) + 1e-9
    if hiss > 0:
        x += rng.standard_normal(len(x)) * (hiss * rms * 0.35)
    if crackle > 0:
        duration = len(x) / rate
        count = int(crackle * duration * 25)
        if count > 0:
            idx = rng.integers(0, len(x), count)
            x[idx] += rng.choice(np.array([-1.0, 1.0]), count) * (0.25 + 0.5 * rng.random(count)) * peak
    if static > 0:
        for _ in range(1 + int(static * 3)):
            length = int(rate * (0.03 + 0.09 * rng.random()))
            start = int(rng.integers(0, max(1, len(x) - length)))
            burst = rng.standard_normal(length)
            burst *= np.exp(-3.0 * np.arange(length) / length)
            x[start:start + length] += burst * (0.15 + 0.45 * static) * peak
    limit = float(np.max(np.abs(x)))
    if limit > 0.98:
        x *= 0.98 / limit
    return x.astype(np.float32)


@app.post("/v1/audio/transcriptions")
def transcriptions(file: UploadFile = File(...), model: str = Form("whisper-1")):
    try:
        audio = file.file.read()
    except Exception as exc:
        raise HTTPException(400, f"Cannot read upload: {exc}")
    if not audio:
        raise HTTPException(400, "Empty audio upload")
    suffix = ".wav"
    if (file.filename or "").lower().endswith(".mp3"):
        suffix = ".mp3"
    try:
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=True) as tmp:
            tmp.write(audio)
            tmp.flush()
            with _lock:
                segments, _info = _stt.transcribe(tmp.name, beam_size=5)
                text = " ".join(s.text.strip() for s in segments).strip()
    except HTTPException:
        raise
    except Exception as exc:
        log.exception("transcription failed")
        raise HTTPException(500, f"Transcription failed: {exc}")
    return {"text": text}


@app.post("/v1/audio/speech")
def speech(body: dict):
    text = (body.get("input") or "").strip() if isinstance(body, dict) else ""
    if not text:
        raise HTTPException(400, "Missing 'input' text")
    if len(text) > MAX_TEXT:
        raise HTTPException(400, "Speech text too long")
    try:
        speed = float(body.get("speed", 1.0))
    except (TypeError, ValueError):
        speed = 1.0
    speed = min(max(speed, 0.5), 2.0)
    try:
        sentence_pause = min(max(float(body.get("sentence_pause", 0.25)), 0.0), 1.0)
        clause_pause = min(max(float(body.get("clause_pause", 0.1)), 0.0), 1.0)
    except (TypeError, ValueError):
        sentence_pause, clause_pause = 0.25, 0.1
    voice = _resolve_voice((body.get("voice") or "").strip())
    fx = body.get("effects") if isinstance(body.get("effects"), dict) else {}
    try:
        with _lock:
            samples, rate = _tts.create(text, voice=voice, speed=speed, lang="en-us",
                                        sentence_pause=sentence_pause, clause_pause=clause_pause)
        samples = _radio_effects(np.asarray(samples, dtype=np.float32), rate, fx,
                                 f"{text}|{voice}|{speed}|{sentence_pause}|{clause_pause}|{sorted(fx.items())}")
        buffer = io.BytesIO()
        sf.write(buffer, samples, rate, format="WAV", subtype="PCM_16")
    except Exception as exc:
        log.exception("synthesis failed")
        raise HTTPException(500, f"Speech synthesis failed: {exc}")
    return Response(content=buffer.getvalue(), media_type="audio/wav")


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8099)
    args = parser.parse_args()
    import uvicorn

    uvicorn.run(app, host=args.host, port=args.port, log_level="info")
