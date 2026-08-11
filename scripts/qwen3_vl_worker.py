#!/usr/bin/env python3
"""Local Qwen3-VL worker for LCU M1.

Protocol: one JSON request per stdin line, one JSON response per stdout line.
Requests:
  {"op":"warmup"}
  {"op":"propose","goal":"...","observation":{...},"image_path":"...optional..."}
Responses always include "ok": true/false.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import traceback
from pathlib import Path
from typing import Any

# Imported lazily from load_model normally; the stopping criteria class needs
# the base at class-definition time, so resolve it once here.
try:
    from transformers import StoppingCriteria
except ImportError:  # pragma: no cover - only relevant in model-less environments
    StoppingCriteria = None  # type: ignore[assignment,misc]

MODEL_DIR = Path(
    os.environ.get(
        "LCU_MODEL_DIR",
        str(Path(__file__).resolve().parents[1] / "models" / "Qwen3-VL-4B-Instruct"),
    )
)

_model = None
_processor = None
_device = None
_load_ms = None


if StoppingCriteria is not None:

    class JsonCompleteStoppingCriteria(StoppingCriteria):
        """Stop generation as soon as the emitted text contains a parseable action.

        The worker's output is a single JSON object; continuing to generate past
        it only burns max_time budget and produces truncation artifacts. The
        criteria reuses `extract_json` so "parseable" means exactly what the
        worker accepts.

        Only the *newly generated* tokens are inspected: the prompt itself
        contains example JSON objects, so scanning the whole sequence would
        either match a prompt example (stop after one token) or never parse
        (prompt text mixed into the slice).
        """

        def __init__(self) -> None:
            self._base_len: int | None = None

        def __call__(self, input_ids, scores, **kwargs) -> bool:
            n = input_ids.shape[-1]
            if self._base_len is None:
                # First call: input is prompt + 1 generated token. Record the
                # boundary a few tokens early so the first generated token is
                # never cut in half; the prompt tail (rule text) contains no
                # '{', so extract_json still finds the model's opening brace.
                self._base_len = max(0, n - 4)
                return False
            try:
                text = _processor.decode(
                    input_ids[0][self._base_len:], skip_special_tokens=True
                )
            except Exception:
                return False
            try:
                extract_json(text)
                return True
            except Exception:
                return False


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def load_model() -> None:
    global _model, _processor, _device, _load_ms
    if _model is not None:
        return
    t0 = time.time()
    import torch
    from transformers import AutoModelForImageTextToText, Qwen3VLProcessor, StoppingCriteria

    if not MODEL_DIR.exists():
        raise FileNotFoundError(f"model dir missing: {MODEL_DIR}")

    # ensure video preprocessor exists (some downloads omit it)
    video_cfg = MODEL_DIR / "video_preprocessor_config.json"
    image_cfg = MODEL_DIR / "preprocessor_config.json"
    if not video_cfg.exists() and image_cfg.exists():
        import json as _json
        pre = _json.loads(image_cfg.read_text())
        video_cfg.write_text(
            _json.dumps(
                {
                    "size": pre.get("size"),
                    "patch_size": pre.get("patch_size", 16),
                    "temporal_patch_size": pre.get("temporal_patch_size", 2),
                    "merge_size": pre.get("merge_size", 2),
                    "image_mean": pre.get("image_mean"),
                    "image_std": pre.get("image_std"),
                    "processor_class": "Qwen3VLProcessor",
                    "video_processor_type": "Qwen3VLVideoProcessor",
                },
                indent=2,
            )
        )

    if torch.backends.mps.is_available():
        _device = torch.device("mps")
        dtype = torch.float16
    else:
        _device = torch.device("cpu")
        dtype = torch.float32

    log(f"loading model from {MODEL_DIR} device={_device} dtype={dtype}")
    _processor = Qwen3VLProcessor.from_pretrained(str(MODEL_DIR), trust_remote_code=True)
    _model = AutoModelForImageTextToText.from_pretrained(
        str(MODEL_DIR),
        dtype=dtype,
        trust_remote_code=True,
        low_cpu_mem_usage=True,
    )
    _model.to(_device)
    _model.eval()
    _load_ms = int((time.time() - t0) * 1000)
    log(f"model ready in {_load_ms} ms")


ACTION_SCHEMA = """
Return ONLY one JSON object (no markdown) with this shape:
{"action": <Action>, "effect_claim": string|null, "expected_effect": string|null, "confidence": number}

Action must be one of:
{"kind":"wait","milliseconds":500}
{"kind":"done","summary":"..."}
{"kind":"fail","reason":"..."}
{"kind":"request_user","reason":"..."}

Rules:
- Never invent element ids.
- Avoid destructive actions (delete/trash/pay/send) unless goal requires.
- One action only.
- Do NOT emit observe. Each request already includes the latest screenshot and elements.
- When the goal is already satisfied or verifiable from the screenshot/title/elements, emit {"kind":"done","summary":"..."}.
- If an element value already contains the text the goal asks for, emit done immediately — do not set_value again.
- Emit done only when the fresh observation proves every part of the entire Goal is complete and no work remains. Last action is history: one successful set_value or one field never proves the whole Goal. Never repeat a satisfied action; take the next unfinished action, or request_user/fail if blocked.
- Do not repeat the same action with the same arguments. Pick done/fail/wait or a different element.
"""

TARGETED_ACTIONS = """
When Elements is empty, these screenshot-targeted actions are also allowed:
{"kind":"targeted","type":"click","x":0.5,"y":0.5,"button":"left"}
{"kind":"targeted","type":"type_text","text":"..."}
{"kind":"targeted","type":"key_combo","keys":["RETURN"]}
Coordinates are normalized to the current window screenshot: x=0 left, x=1 right,
y=0 top, y=1 bottom. Use type_text only after the intended field is focused.
On macOS, the only supported key_combo is exactly ["RETURN"] or ["ENTER"].
Use RETURN after filling a search/filter field when results require submission.
"""


def select_elements(elements: list[dict[str, Any]], goal: str) -> list[dict[str, Any]]:
    """Keep the small, useful part of a noisy app-wide AX tree."""
    quoted_targets = re.findall(r'[“"「](.*?)[”"」]', goal)

    def score(item: tuple[int, dict[str, Any]]) -> tuple[int, int]:
        index, element = item
        role = str(element.get("role") or "")
        actions = element.get("actions") or []
        text = f"{element.get('label') or ''} {element.get('value') or ''}"
        frame = element.get("frame") or {}
        points = 0
        if any(target and target in text for target in quoted_targets):
            points += 100
        if any(name in role for name in ("TextField", "TextArea", "SearchField", "ComboBox")):
            points += 60
        if any(action in actions for action in ("AXPress", "AXConfirm")):
            points += 20
        if isinstance(frame, dict):
            width, height = frame.get("width") or 0, frame.get("height") or 0
        elif isinstance(frame, list) and len(frame) >= 4:
            width, height = frame[2], frame[3]
        else:
            width, height = 0, 0
        if float(width) > 0 and float(height) > 0:
            points += 10
        if role in ("AXMenu", "AXMenuItem", "AXMenuBar", "AXMenuBarItem"):
            points -= 40
        return points, -index

    ranked = sorted(enumerate(elements), key=score, reverse=True)
    keep = {index for index, _ in ranked[:28]}
    return [element for index, element in enumerate(elements) if index in keep]


def build_prompt(
    goal: str,
    observation: dict[str, Any],
    *,
    step: int | None = None,
    last_action_summary: str | None = None,
) -> str:
    elements = observation.get("elements") or []
    compact = []
    selected = select_elements(elements, goal)
    # Cap tree size hard: VL + long AX on MPS often stalls and emits truncated JSON.
    for e in selected:
        compact.append(
            {
                "id": e.get("id"),
                "role": (e.get("role") or "")[:32],
                "label": (e.get("label") or "")[:48] or None,
                "value": (e.get("value") or "")[:48] or None,
                "actions": e.get("actions") or [],
            }
        )
    element_ids = [str(e["id"]) for e in compact if e.get("id")]
    is_chrome = "chrome" in str(observation.get("app_id") or "").lower()
    navigation_action = (
        '\nChrome navigation is allowed with an explicit HTTP(S) URL:\n'
        '{"kind":"semantic","type":"navigate","url":"https://example.com"}'
        if is_chrome
        else ""
    )
    if element_ids:
        example_id = json.dumps(element_ids[0], ensure_ascii=False)
        focus_action = (
            f'{{"kind":"semantic","type":"focus","element_id":{example_id}}}\n'
            if is_chrome
            else ""
        )
        semantic_actions = (
            "Semantic actions are also allowed; element_id must be copied from "
            f"Valid element_ids={json.dumps(element_ids, ensure_ascii=False)}:\n"
            f'{{"kind":"semantic","type":"invoke","element_id":{example_id}}}\n'
            f'{{"kind":"semantic","type":"set_value","element_id":{example_id},"value":"..."}}\n'
            f"{focus_action}"
            '{"kind":"semantic","type":"scroll","element_id":null,"delta_x":0,"delta_y":-0.3}\n'
            "Elements contains usable AX controls, so targeted click/type_text is "
            "forbidden for this observation. Use semantic actions. After setting "
            "a search/filter field, invoke that same element only when its actions "
            "include AXConfirm and the results have not refreshed."
            f"{navigation_action}"
        )
    else:
        semantic_actions = (
            "Elements is empty. Element-bound semantic actions are forbidden because no valid "
            "element_id exists. Use targeted click/type_text from the screenshot, "
            "or fail/request_user if safe progress is impossible.\n"
            f"{TARGETED_ACTIONS}{navigation_action}"
        )
    step_line = f"Step: {step}\n" if step is not None else ""
    last_line = (
        f"Last action: {last_action_summary}\n"
        if last_action_summary
        else "Last action: (none — first step)\n"
    )
    # Schema first so early tokens are valid JSON even if generation is cut short.
    return (
        f"{ACTION_SCHEMA}\n"
        f"{semantic_actions}\n"
        f"Goal: {goal}\n"
        f"{step_line}"
        f"{last_line}"
        f"Current observation is fresh (screenshot + elements below).\n"
        f"App: {observation.get('app_id')} title={observation.get('window_title')}\n"
        f"Elements:\n{json.dumps(compact, ensure_ascii=False)}\n"
        f"Output ONE complete JSON object only. Start with {{\"action\": and close all braces."
    )


def normalize_action_payload(obj: dict[str, Any]) -> dict[str, Any]:
    """Coerce common model-sloppy shapes into {action: {kind, ...}, ...}.

    Seen in the wild:
      {"action": "semantic", "type": "invoke", "element_id": "el_0"}
    Should become:
      {"action": {"kind": "semantic", "type": "invoke", "element_id": "el_0"}}
    """
    if not isinstance(obj, dict):
        return obj
    action = obj.get("action", obj)
    if isinstance(action, str) and action.strip():
        # Flattened: kind was stored in "action", fields live at top level.
        kind = action.strip()
        rebuilt: dict[str, Any] = {"kind": kind}
        for k in (
            "type",
            "element_id",
            "url",
            "value",
            "delta_x",
            "delta_y",
            "milliseconds",
            "summary",
            "reason",
            "text",
            "x",
            "y",
            "button",
            "keys",
        ):
            if k in obj and k != "action":
                rebuilt[k] = obj[k]
        out = {k: v for k, v in obj.items() if k not in rebuilt and k != "action"}
        out["action"] = rebuilt
        return out
    if isinstance(action, dict):
        return obj
    # Bare action object without wrapper.
    if "kind" in obj and "action" not in obj:
        return {"action": obj}
    return obj


def _ends_inside_string(text: str) -> bool:
    """True if `text` ends in the middle of an unterminated JSON string.

    Scans honoring backslash escapes so a literal `\"` inside a string does
    not flip the in-string state.
    """
    in_str = False
    escaped = False
    for ch in text:
        if escaped:
            escaped = False
            continue
        if ch == "\\":
            escaped = True
        elif ch == '"':
            in_str = not in_str
    return in_str


def extract_json(text: str) -> dict[str, Any]:
    text = text.strip()
    if text.startswith("```"):
        text = text.strip("`")
        if text.startswith("json"):
            text = text[4:].strip()
    def _valid(obj: dict[str, Any]) -> dict[str, Any]:
        obj = normalize_action_payload(obj)
        action = obj.get("action", obj)
        if isinstance(action, dict):
            kind = action.get("kind")
            if not kind:
                raise ValueError("incomplete action kind")
            return obj
        raise ValueError(f"action is not an object after normalize: {type(action).__name__}")

    start = text.find("{")
    end = text.rfind("}")
    if start >= 0 and end > start:
        try:
            return _valid(json.loads(text[start : end + 1]))
        except Exception:
            pass
    # Truncated generation (common on short max_new_tokens / max_time).
    chunk = text[start:] if start >= 0 else text
    if _ends_inside_string(chunk):
        # A cut inside a string value cannot be repaired safely: padding would
        # turn it into valid JSON with a silently truncated value (e.g.
        # "value":"submit applic -> "applic"), and that mangled action would be
        # executed as if it were the model's real intent. Fail to trigger retry.
        raise ValueError(
            f"no json object in model output (truncated inside a string): {text[:200]}"
        )
    for closer in ("}}", '"}', "}}}", '"}}'):
        try:
            return _valid(json.loads(chunk + closer))
        except Exception:
            continue
    raise ValueError(f"no json object in model output: {text[:200]}")


def _resize_image(path: str) -> str:
    """Downscale large window captures so MPS generate does not hang for minutes."""
    from PIL import Image

    # Smaller default: large captures dominate MPS prefill and starve generation.
    # 384 keeps enough visual fidelity for window-level actions while roughly
    # halving the image token cost of a 512px capture.
    max_side = int(os.environ.get("LCU_VLM_MAX_IMAGE", "384"))
    img = Image.open(path).convert("RGB")
    w, h = img.size
    scale = min(1.0, float(max_side) / float(max(w, h)))
    if scale < 1.0:
        nw, nh = max(1, int(w * scale)), max(1, int(h * scale))
        img = img.resize((nw, nh), Image.Resampling.BILINEAR)
        out = Path(path).with_suffix(".vlm.jpg")
        img.save(out, format="JPEG", quality=85)
        try:
            os.chmod(out, 0o600)
        except OSError:
            pass
        log(f"resized image {w}x{h} -> {nw}x{nh} path={out}")
        return str(out)
    return path


def _cleanup_resized(resized_path: str | None, image_path: str | None) -> None:
    if resized_path and resized_path != image_path:
        try:
            Path(resized_path).unlink(missing_ok=True)
        except OSError:
            pass


def propose(req: dict[str, Any]) -> dict[str, Any]:
    load_model()
    import torch

    goal = req.get("goal") or ""
    observation = req.get("observation") or {}
    image_path = req.get("image_path")
    step = req.get("step")
    if step is not None:
        try:
            step = int(step)
        except (TypeError, ValueError):
            step = None
    last_action_summary = req.get("last_action_summary")
    if last_action_summary is not None:
        last_action_summary = str(last_action_summary)[:240]
    prompt = build_prompt(
        goal,
        observation,
        step=step,
        last_action_summary=last_action_summary,
    )
    # Single total wall budget for one propose (attempt + optional JSON retry).
    # Retry must consume remaining time only — never re-grant a full 180s.
    # One action JSON is well under 200 tokens; a larger cap only lengthens
    # generation that the JSON-complete criteria will cut short anyway.
    max_new = int(req.get("max_new_tokens") or int(os.environ.get("LCU_VLM_MAX_NEW", "192")))
    total_budget = float(
        req.get("max_time")
        or os.environ.get("LCU_VLM_MAX_TIME")
        or os.environ.get("LCU_VLM_PROPOSE_SECS")
        or "180"
    )
    total_budget = max(15.0, total_budget)

    content: list[dict[str, Any]] = [{"type": "text", "text": prompt}]
    resized_path = None
    # Product default: multimodal. Parent Rust must env_remove LCU_VLM_NO_IMAGE in
    # release; only explicit request no_image=true is a debug opt-out.
    force_no = req.get("no_image") is True
    use_image = not force_no
    try:
        if use_image and image_path and Path(image_path).exists():
            try:
                resized_path = _resize_image(str(image_path))
                content.insert(0, {"type": "image", "image": resized_path})
                log(f"propose mode=multimodal image={resized_path}")
            except Exception as e:
                raise RuntimeError(
                    f"image preprocess failed (product requires screenshot): {e}"
                ) from e
        elif use_image:
            raise RuntimeError(
                "product VLM requires image_path; missing or unreadable screenshot "
                "(debug only: set request no_image=true)"
            )
        else:
            log("propose mode=text-only (explicit request no_image)")

        messages = [{"role": "user", "content": content}]

        log(
            f"propose start goal_len={len(goal)} elements={len(observation.get('elements') or [])} "
            f"step={step} last={last_action_summary!r} max_new={max_new} total_budget={total_budget}"
        )
        t0 = time.time()
        inputs = _processor.apply_chat_template(
            messages,
            tokenize=True,
            add_generation_prompt=True,
            return_dict=True,
            return_tensors="pt",
        )
        inputs = {k: v.to(_device) if hasattr(v, "to") else v for k, v in inputs.items()}
        n_tokens = int(inputs["input_ids"].shape[-1]) if "input_ids" in inputs else -1
        log(f"propose tokens={n_tokens} generating…")

        def _remaining() -> float:
            return max(5.0, total_budget - (time.time() - t0))

        def _generate(n_new: int, wall: float) -> str:
            criteria = None
            if StoppingCriteria is not None:
                criteria = [JsonCompleteStoppingCriteria()]
            with torch.inference_mode():
                out_ids = _model.generate(
                    **inputs,
                    max_new_tokens=n_new,
                    do_sample=False,
                    max_time=max(5.0, wall),
                    stopping_criteria=criteria,
                )
            prompt_len = inputs["input_ids"].shape[-1]
            gen = out_ids[0][prompt_len:]
            return _processor.batch_decode([gen], skip_special_tokens=True)[0]

        text = _generate(max_new, _remaining())
        latency_ms = int((time.time() - t0) * 1000)
        log(
            f"propose done latency_ms={latency_ms} remaining={_remaining():.1f}s "
            f"out_chars={len(text)} raw={text[:200]!r}"
        )

        try:
            parsed = extract_json(text)
        except Exception as e1:
            remaining = _remaining()
            if remaining < 10.0:
                log(f"json parse failed ({e1}); no time left for retry remaining={remaining:.1f}s")
                raise RuntimeError(
                    f"VLM output illegal / unparseable: {e1}; raw={text[:300]!r}"
                ) from e1
            # One re-generate using only remaining budget (not a fresh 180s grant).
            log(
                f"json parse failed ({e1}); retrying once with more tokens "
                f"remaining_budget={remaining:.1f}s"
            )
            text2 = _generate(max(max_new, 384), remaining)
            latency_ms = int((time.time() - t0) * 1000)
            log(f"propose retry done latency_ms={latency_ms} out_chars={len(text2)}")
            try:
                parsed = extract_json(text2)
                text = text2
            except Exception as e2:
                log(f"json parse failed after retry ({e2}); refusing salvage")
                raise RuntimeError(
                    f"VLM output illegal / unparseable: {e2}; raw={text2[:300]!r}"
                ) from e2
        action = parsed.get("action", parsed)
        log(f"propose action={action!r} total_latency_ms={latency_ms}")
        return {
            "ok": True,
            "raw_text": text,
            "action": action,
            "effect_claim": parsed.get("effect_claim"),
            "expected_effect": parsed.get("expected_effect"),
            "confidence": parsed.get("confidence", 0.5),
            "latency_ms": latency_ms,
            "load_ms": _load_ms,
            "device": str(_device),
            "model_dir": str(MODEL_DIR),
            "salvaged": False,
            "step": step,
            "last_action_summary": last_action_summary,
        }
    finally:
        _cleanup_resized(resized_path, image_path)


def _warm_prefill() -> None:
    """Pay the MPS first-inference compile cost inside warmup.

    The first generate() on a freshly loaded model compiles kernels and can
    take 1.5–3 minutes — longer than the propose budget, so the first task
    after a restart was failing on a truncated 17-char output almost every
    time. A tiny warm generation absorbs that cost up front.
    """
    global _processor, _device
    try:
        import torch

        messages = [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        inputs = _processor.apply_chat_template(
            messages,
            tokenize=True,
            add_generation_prompt=True,
            return_dict=True,
            return_tensors="pt",
        )
        inputs = {k: v.to(_device) if hasattr(v, "to") else v for k, v in inputs.items()}
        with torch.inference_mode():
            _model.generate(**inputs, max_new_tokens=4, do_sample=False)
        log("warm prefill done")
    except Exception as e:
        log(f"warm prefill skipped: {e}")


def handle(req: dict[str, Any]) -> dict[str, Any]:
    op = req.get("op")
    if op == "warmup":
        load_model()
        _warm_prefill()
        return {
            "ok": True,
            "load_ms": _load_ms,
            "device": str(_device),
            "model_dir": str(MODEL_DIR),
        }
    if op == "propose":
        return propose(req)
    if op == "ping":
        return {"ok": True, "pong": True}
    return {"ok": False, "error": f"unknown op {op}"}


def main() -> None:
    if "--self-check" in sys.argv:
        parsed = extract_json(
            '{"action":"semantic","type":"navigate","url":"https://example.com/path"}'
        )
        assert parsed["action"] == {
            "kind": "semantic",
            "type": "navigate",
            "url": "https://example.com/path",
        }
        sample = {"elements": [{"id": "e1", "role": "AXTextField"}]}
        mac_prompt = build_prompt("fill field", {**sample, "app_id": "com.apple.TextEdit"})
        chrome_prompt = build_prompt("fill field", {**sample, "app_id": "com.google.Chrome"})
        assert '"type":"focus"' not in mac_prompt
        assert '"type":"focus"' in chrome_prompt
        mac_empty_prompt = build_prompt("fill field", {"app_id": "com.apple.TextEdit", "elements": []})
        assert 'only supported key_combo is exactly ["RETURN"] or ["ENTER"]' in mac_empty_prompt
        print("parser/prompt self-check ok")
        return
    log(f"qwen3_vl_worker starting model_dir={MODEL_DIR}")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            resp = handle(req)
        except Exception as e:
            resp = {
                "ok": False,
                "error": str(e),
                "traceback": traceback.format_exc()[-2000:],
            }
        print(json.dumps(resp, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
