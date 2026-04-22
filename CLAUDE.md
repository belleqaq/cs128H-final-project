# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

## 5. No Magic Numbers

**Every literal must be traceable to a physical quantity or a named constant.**

- Never hardcode thresholds, multipliers, or limits as bare literals in logic.
- Extract them as `const` with a descriptive name that explains *what* it controls.
- Add a comment explaining *why* this value was chosen — ideally referencing the physical quantity it derives from (e.g. collision radius, tile size, tick rate).
- If a value is a multiple of another parameter (e.g. `radius * 3.0`), the multiplier itself must be a named constant with rationale.
- When in doubt: if someone reading the code would ask "where does this number come from?", it needs a name and a comment.

## 6. Debug Toggle Log Protocol

**First-time debug on any toggle → write its log output logic FIRST.**

When starting the first debug task for a pipeline toggle:
1. Add comprehensive `eprintln` logging covering every key parameter and decision point in that toggle's code path. Ensure the log output is structured enough to diagnose issues without guessing. **Keep log output concise** — one line per decision point, no redundant information. Target: ≤ 30 lines per toggle per generation for the default 6-room config.
2. Record the log prefix (e.g. `[doors]`) in `output/WFC_DESIGN.md` under a "Debug Logs" section.
3. In every subsequent debug iteration, **read the log output first** before asking the user for more information. Only ask questions to resolve ambiguities that the log cannot answer.
4. Never guess at root causes — use log evidence + user description. If the two contradict, ask the user to re-confirm.

**Default map config** (when user does not specify): 3 Normal + 2 Toilet + 1 Trash = 6 rooms.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.
