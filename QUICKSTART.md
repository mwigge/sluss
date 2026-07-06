# sluss quickstart

from zero to a live gate on one repo, every click included. ~15 minutes.

## 0. build

```
git clone https://github.com/mwigge/sluss && cd sluss
cargo build --release
```

## 1. register the github app

sluss acts as a github app — that's what makes check runs (the real merge
gate) possible.

1. go to https://github.com/settings/apps/new
2. name: anything unique (e.g. `sluss-<your-user>`), homepage: whatever
3. webhook: check "active", url `https://your-host/webhook/github` — or an
   `https://example.invalid` placeholder if you'll use `gh webhook forward`
   while testing (see step 4). set a webhook secret and keep it
4. permissions: **checks: read & write**, **pull requests: read & write**,
   **contents: read** (metadata comes automatically)
5. subscribe to events: **pull request**
6. create, then on the app page: note the **App ID**, click **generate a
   private key** — a `.pem` downloads
7. "install app" in the left menu → your account → all repos or pick some

## 2. configure

```
export SLUSS_GITHUB_APP_ID=<app id>
export SLUSS_GITHUB_APP_KEY_PATH=/path/to/downloaded.pem
export SLUSS_GITHUB_WEBHOOK_SECRET=<the secret from step 1.3>

# the reviewer — genai picks the provider from the model name + its key env:
export SLUSS_MODEL=claude-sonnet-5        # default
export ANTHROPIC_API_KEY=...              # or MINIMAX_API_KEY + SLUSS_MODEL=MiniMax-M2, etc

# the gate (defaults shown):
export SLUSS_MIN_CONFIDENCE=0.8           # approvals below this become comments
export SLUSS_REQUIRE_CI_GREEN=true        # never approve on red/absent CI
```

## 3. run

```
./target/release/sluss serve
```

listens on `127.0.0.1:8907` (`SLUSS_ADDR` to change). audit db goes to
`~/.local/share/sluss/sluss.db` (`SLUSS_DB` to change).

## 4. get webhooks to it

two options:

- **testing, no public url**: `gh extension install cli/gh-webhook`, then
  ```
  gh webhook forward --repo=you/repo --events=pull_request \
    --url=http://127.0.0.1:8907/webhook/github --secret=$SLUSS_GITHUB_WEBHOOK_SECRET
  ```
  (dev-grade: the websocket drops occasionally — wrap it in a retry loop)
- **for real**: expose the daemon (reverse proxy, cloudflared, whatever
  you trust) and set that url as the app's webhook url in step 1.3

## 5. watch it work

open or update a PR on an installed repo, then:

```
./target/release/sluss log you/repo 42    # the decision trail for one PR
./target/release/sluss dash               # the live dashboard
```

on the PR you'll see a `sluss` check run (with the rationale and line
annotations) and a review from your app.

## 6. make the gate real

repo → settings → branches → branch protection for `main` → require status
checks → add `sluss`. now a `request_changes` verdict actually blocks the
merge button, and an approval is a real approval.

## gitlab

same daemon handles MRs: set `SLUSS_GITLAB_TOKEN` (+ `SLUSS_GITLAB_URL` for
self-hosted) and point a project webhook (merge request events, secret
token = `SLUSS_GITLAB_WEBHOOK_TOKEN`) at `/webhook/gitlab`. the gate there
is the `sluss` commit status + MR approvals.

## troubleshooting

- daemon warns `token-only auth` → you set `SLUSS_GITHUB_TOKEN` instead of
  app credentials; reviews work but check runs are rejected by github
- `app is not installed on owner/repo` in `sluss log` → step 1.7
- pipeline errors are never lost: `sluss log` shows exactly which step
  failed and why — that's the point of the whole thing
