# sluss

sluss (swedish: *canal lock*) — a gate for pull requests. a PR sails in, gets inspected, and the gate either opens or stays shut. every step of that leaves a trace.

## what it is

a bot that works through PRs and MRs so you don't have to babysit them:

- reads the diff, the description, the discussion
- checks what CI/tests say
- decides: approve, request changes, or just comment
- posts line-level annotations where it has something to say
- and the whole point: **every action is traceable**. who decided what, based on which commit, with what rationale, at what confidence — all of it queryable after the fact

github and gitlab both, since work lives in both places.

## the one rule

**the model proposes, the gate disposes.**

the LLM never touches the forge directly. it produces a structured `Decision` (verdict + rationale + annotations + confidence). that decision goes into the audit log *first*, verbatim. then a deterministic, unit-tested `GatePolicy` — plain rust, no model in the loop — decides what actually gets enacted. red CI? approval downgraded to a comment. confidence too low? downgraded, with the reason recorded. you can read the gate in one sitting and test it like any other function.

this split is borrowed from people running this in production (intercom wrote a good piece on it), and the harness idea in general owes a lot to [openai's harness engineering post](https://openai.com/index/harness-engineering/) and [pr-inbox](https://github.com/jmprieur/pr-inbox).

## traceability

three layers, so "why did the bot approve #42?" is always answerable:

1. **append-only event log** — sqlite, and the schema itself refuses UPDATE and DELETE (triggers raise). webhook received, snapshot taken, decision proposed, gate outcome, action posted — one row each, never rewritten. re-reviewing appends, nothing is overwritten (stolen with pride from pr-inbox)
2. **check runs as the public record** — on github every outcome becomes a check run pinned to the exact head commit, with summary, rationale and annotations in the diff. branch protection turns that check into the actual merge gate. gitlab gets the same via external status checks + approvals api
3. sqlite is the long-term copy — github archives check data after ~400 days, our log doesn't expire

## layout

```
crates/
  sluss-core     domain types + the deterministic gate (done, tested)
  sluss-audit    append-only sqlite store (done, tested)
  sluss-github   octocrab-based: snapshot PRs, publish check runs (stubs)
  sluss-gitlab   hand-written webhook payloads, MR side (parsing done, api stubs)
  sluss-llm      genai-based reviewer -> structured Decision (stub)
  sluss          the daemon: axum webhook receiver (working)
```

## status

early. what works today:

- [x] webhook receiver with signature verification (github hmac, gitlab token, constant-time)
- [x] every verified webhook lands in the audit log before anything else happens
- [x] the gate, with tests
- [ ] snapshot: pull diff + CI state from the forge
- [ ] reviewer: genai call with structured output
- [ ] publish: check runs + reviews on github, status checks + notes on gitlab
- [ ] a `sluss log <repo> <nr>` command to read the audit trail

## running

```
export SLUSS_GITHUB_WEBHOOK_SECRET=...   # from your github app
export SLUSS_GITLAB_WEBHOOK_TOKEN=...    # from your gitlab webhook config
export SLUSS_DB=sluss.db                 # default
export SLUSS_ADDR=127.0.0.1:8907         # default

cargo run -p sluss
```

point a github app (pull_request + check_suite events) or a gitlab webhook (merge request events) at `/webhook/github` or `/webhook/gitlab`.

## why not just use an existing tool

[pr-agent](https://github.com/qodo-ai/pr-agent) is good and covers the review-and-comment part on both forges — if that's all you need, use that. but it deliberately won't approve autonomously, and its trace is basically "the comments it left". the combination of *real approve/block power* and *a proper audit trail* didn't exist, so: sluss.
