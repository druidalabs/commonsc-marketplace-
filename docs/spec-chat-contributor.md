# Spec: chat-driven contribution inside the app (build → test → visualise → submit)

Status: draft · Owner: DruidaLabs · Date: 2026-06

## Goal

Let a person (or an agent acting for them) author a genomic test **conversationally, inside the desktop app** — describe it in natural language, watch it get built, tested against their own sample, and visualised, then submit it through the contributor pipeline. The agent-first contract already exists for *external* agents; this brings the same loop *inside* the app for everyone, no CLI required.

## What already exists (the rails)

This is mostly wiring onto shipped capability:
- **Test:** `run_local_bundle` runs a project directory against the active sample in the sandbox, locally (v0.1.7).
- **Real data:** the requiredRsids data plane (session genome cache, persisted) hands an algorithm any SNP it declares — so a generated test actually resolves.
- **Visualise:** the block renderer (`ui/src/blocks.tsx`) + the block gallery as the closed output vocabulary.
- **Ground truth for the loop:** the execution gate + `result.schema.json` envelope validation give a pass/fail signal the model can iterate against.
- **Contract to target:** manifest / genomic-io / result schemas, worked examples, `llms.txt` — exactly an LLM's grounding.
- **Submit:** register → sign → publish (devkit / the HTTP API), with the server-side execution gate.

New pieces: an LLM client, the agentic loop, in-app signing/publish, and the chat UI.

## The agentic loop

```
describe ──▶ LLM generates manifest.template.json + <pkg>/main.py
        ──▶ write to a temp project dir (Tauri fs)
        ──▶ run_local_bundle against the user's sample
        ──▶ pass {execution error | envelope-conformance errors} back to the LLM
        ──▶ revise ── repeat until it runs clean + conforms
        ──▶ preview the result (ResultBlocks)
        ──▶ submit (register/sign/publish)
```

The loop converges against **machine signals** (does it run? does the envelope conform? does it use only closed block kinds?), which the model is good at fixing. It's a bounded codegen task (genotype → interpretation → result blocks) with strong grounding (schemas + examples).

## The privacy boundary — the crux, and the trust risk

The DNA must never reach the cloud LLM. Architect the boundary so the model only ever receives:
- the user's task description,
- the schemas / examples / block vocabulary,
- the generated code,
- **execution errors and envelope-conformance results** (e.g. "ValueError on line 12", "block kind `foo` is not allowed", "missing field `summary`").

It must **never** receive: the user's variants, genotypes, or the *result values* (which encode the user's genome). The test runs **locally** via `run_local_bundle`; only the user sees the rendered result. Critically, the loop iterates on *execution/envelope* feedback, **not** on the user's actual output — so "improve the result" must be driven by conformance, never by sending the result back. Get this wrong and the core promise ("your DNA never leaves") breaks.

Operationally: the LLM call is the only egress, gated behind the network shield with explicit consent, and it carries no genomic data. This preserves local-first while enabling cloud codegen.

## The other new pieces

- **Generate + write:** the model's manifest + `main.py` are written to a temp project dir; `requiredRsids` from `--rsid`-style declaration drive the data plane. Reuse `run_local_bundle` (directory-based) to execute.
- **Sign + publish from the app:** two options — (a) shell out to the installed `commonsc-devkit` binary (reuse the proven signing/JWT path; requires the toolkit installed), or (b) a minimal in-app ed25519 signer in `src-tauri` mirroring the devkit canonicalisation. Recommend (a) first (no duplication); revisit (b) if we don't want the binary dependency.
- **Chat UI:** a panel with the conversation, a collapsible code view (editable — let the human take over), a run/preview pane (ResultBlocks + timing + errors), and a submit button that surfaces the review-queue status.

## Phasing

- **D1 — the loop, author+test+preview (no submit).** LLM client + generate → write temp project → `run_local_bundle` → feed errors back → converge → preview. Privacy boundary enforced from day one. This alone is a huge authoring accelerator.
- **D2 — submit from the app.** Wire register/sign/publish (shell devkit), surface review status. End-to-end in-app contribution.
- **D3 — polish.** Editable code, regenerate/iterate controls, diff view, "explain what this does", multi-SNP/PRS templates.

## Open decisions

1. **LLM provider + key.** Anthropic API. Whose key — DruidaLabs-subsidised (simplest for users, we bear cost) or user-supplied (no cost to us, friction)? Likely subsidised with a quota.
2. **Network gating.** The LLM call is egress — how prominent the consent, and does the shield show "talking to the model (no DNA)" distinctly from "pulling the catalog".
3. **Local vs cloud model.** Cloud now (quality); a bundled local model later would close the boundary entirely but is heavy — not near-term.
4. **Signing path.** Shell to devkit (a) vs in-app signer (b).

## Pessimistic caveats

- **Privacy-boundary discipline is the whole ballgame** — it must be impossible for variants/results to enter an LLM prompt. Worth an explicit, tested seam (the prompt builder takes only the allowed inputs; the result object is never in scope).
- **Cost/latency** of cloud calls; needs a quota story.
- **Codegen quality** is good for this narrow, well-specified task with the schemas + examples as grounding + the execution gate as a corrector — but it will occasionally produce wrong *science* (the sandbox can't catch a bad interpretation). Human review + the "trait curiosity, no medical claims" policy remain the backstop.
- **Scope:** a real feature (LLM client + loop + signing + UI), bounded but not a weekend. D1 is the high-value core.

Relates to [[project_commonsense_data_plane]] (makes generated tests resolve), [[project_commonsense_contributor_dogfood]] (the contribution ethos), and `spec-unified-execution.md` (the execution rails this builds on).
