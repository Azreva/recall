# Claude Code instructions

@AGENT.md

The imported [shared guide](AGENT.md) is the repository-wide contribution policy. Keep common instructions there; this file provides the Claude Code entry point.

## Working in this repository

- Inspect the relevant sources and tests before editing. Present a short plan for multi-component or correctness-sensitive changes, then make the smallest coherent change.
- Respect the contributor's execution permissions. Do not install a missing toolchain, access remote systems, bypass tool approvals, or run a build on a machine the contributor has excluded.
- If delegating work, give each subagent a narrow scope and the same ownership, security, and verification constraints. Avoid concurrent edits to the same files; review delegated results yourself.
- Preserve unrelated changes. Do not commit, push, deploy, or alter repository permissions unless explicitly requested.
- Use [the contribution checks](CONTRIBUTING.md) where available. Report exact results and clearly identify unrun checks; do not present an agent's confidence as test evidence.
- Target Linux only. Keep internal conversations, terminal transcripts, and private deployment records out of documentation; follow the shared guide's privacy boundaries.
- Leave a concise, human-reviewable handoff with behavior changes, affected files, verification, and unresolved risks.
