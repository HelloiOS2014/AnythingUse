# Computer Use Reference Notes

Reviewed: 2026-08-14
Status: **reference only; non-normative**

AnythingUse product behavior is defined only by the
[Execution Contract](execution-contract.md). This file records facts and useful
patterns from other products. A referenced capability does not become an
AnythingUse dependency, backend, or requirement.

## Evidence labels

- **Official**: stated in linked product documentation.
- **Installed contract**: observed in the versioned Codex Computer Use Skill on
  this development machine; not a promise about private implementation.
- **Implemented** and **runtime-verified** belong in
  [Delivery status](status.md), not this reference.

## Frozen AnythingUse boundaries

External products do not override these owner decisions:

- humans and Agents share the public `lcu` CLI and Agent Skill;
- external Agent and local VLM remain selectable per task;
- both Actors share one Runtime, queue, contract, safety path, and Operator;
- Chrome uses the existing extension and Native Messaging against the user's
  real Chrome profile;
- macOS uses native application/window control;
- no MCP or Playwright Computer Use control surface;
- no application-, contact-, or acceptance-specific policy.

## Codex / ChatGPT Computer Use

**Layer:** end-user Computer Use product.

**Official:** [OpenAI Computer Use documentation](https://learn.chatgpt.com/docs/computer-use)
describes permissioned app access, saved app permissions, user stop/takeover,
cross-application work, a Chrome extension, and sensitive or disruptive action
prompts. It documents background-capable macOS tasks but does not promise that
every application or same-browser workflow is non-interfering.

**Installed contract:** the bundled Computer Use Skill exposes app-scoped state
capture, Accessibility elements, screenshot coordinates, pointer and keyboard
actions, text entry, scrolling, and selection. It refreshes app state after
actions and rejects stale element assumptions. This establishes the shape of a
mature one-action/fresh-state loop, not an AnythingUse implementation choice.

## OpenClaw Computer Use

**Layer:** Agent runtime and Computer Use tool.

**Official:** [OpenClaw Computer Use documentation](https://docs.openclaw.ai/nodes/computer-use)
describes one action per call, coordinate binding to the latest screenshot
frame, display identity, fresh screenshots after input, and a common
pointer/keyboard surface.

Its separate [browser tool](https://docs.openclaw.ai/tools/browser) has managed
profile and tab lifecycle features. Those browser features are not properties
of `computer.act` and are not an AnythingUse dependency.

## Anthropic Computer Use

**Layer/status:** model tool contract, Beta.

**Official:** [Anthropic Computer Use documentation](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)
describes a client-executed screenshot/action/result loop, standard coordinate
and keyboard actions, updated screenshots after actions, iteration limits, and
human-oversight guidance. Its container reference is not a macOS execution
backend for AnythingUse.

## Gemini Computer Use

**Layer/status:** model tool contract, Preview.

**Official:** [Gemini Computer Use documentation](https://ai.google.dev/gemini-api/docs/computer-use)
describes screenshot-to-action calls whose safety decision can be allowed,
confirmation-required, or blocked. The client owns execution and returns the
next screenshot. AnythingUse adopts the separation of proposal, safety, and
execution, not the API or browser examples.

## UI-TARS Desktop SDK

**Layer/status:** pluggable GUI-agent SDK, experimental.

**Official:** [UI-TARS SDK documentation](https://github.com/bytedance/UI-TARS-desktop/blob/main/docs/sdk.md)
describes separate model/operator roles, loop limits, and cancellation. It does
not define AnythingUse's application permissions or risk policy.

## Peekaboo and CUA

**Layer:** lower-level execution capability references.

[Peekaboo](https://github.com/openclaw/Peekaboo) documents macOS screenshot,
Accessibility inspection, opaque element identifiers, and window/input
operations. Some targets still require a foreground fallback.

[CUA](https://github.com/trycua/cua) claims cross-platform background native-app
control with platform limitations. It is neither a selected backend nor proof
that a specific application can be controlled in the background.

## Common lessons retained

The useful common denominator is small:

1. observe the real target;
2. bind coordinates/elements to that observation;
3. propose and execute one action;
4. re-observe after UI change;
5. separate application access from risky consequences;
6. let the user stop or take over;
7. hide platform-specific mechanisms behind one public tool contract.

The products differ on background execution, browser isolation, approval
frequency, safety policy, and platform implementation. Those differences are
resolved by the AnythingUse [Execution Contract](execution-contract.md), not by
copying whichever product was reviewed most recently.
