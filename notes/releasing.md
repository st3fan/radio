# Releasing ("shipping")

Shipping = **publishing a GitHub Release**. That is the only event that
builds `.deb`s; nothing ships from pushes or merges.

## 1. Pick the version

`X.Y.Z`, semver-ish while pre-1.0: minor for features, patch for fixes.
One version line for everything (radiod and the release artifacts).

## 2. Version-bump PR against main

- `service/Cargo.toml`: `version = "X.Y.Z"`.
- Run `cargo test` in `service/` (refreshes `Cargo.lock`; must be green).
- Open the PR, merge it. The release workflow **asserts the tag matches
  Cargo.toml** and fails the release otherwise — the bump must land
  first.

## 3. Publish the release (from main)

- **Tag:** `vX.Y.Z` — the leading `v` matters (the workflow strips it).
- **Title:** `X.Y.Z — <a few words>`.
- **Notes:** a short story of what the release *means*, followed by the
  GitHub-generated changelog (the commit/PR list):
  - CLI: `gh release create vX.Y.Z --target main --title "…" --notes
    "<story>" --generate-notes` — `--generate-notes` appends the
    generated changelog after the story, same as the **"Generate
    release notes"** button in the web UI.
  - UI: write the story, then press that button.

## 4. Wait for the workflow (~5 min)

Builds `radiod_X.Y.Z-1_{amd64,arm64,armhf}.deb`, signs a provenance
attestation for each, attaches them plus `SHA256SUMS`. Watch with
`gh run watch` or the Actions tab. One flaky leg does not cancel the
others; rerun failed jobs from the run page if the infrastructure
hiccups.

## 5. Verify and upgrade the radio

On the radio (muzak), as root:

```
base=https://github.com/st3fan/radio/releases/download/vX.Y.Z
curl -sLO $base/radiod_X.Y.Z-1_arm64.deb -sLO $base/SHA256SUMS
grep arm64 SHA256SUMS | sha256sum -c -
apt-get install -y ./radiod_X.Y.Z-1_arm64.deb
```

- Upgrades **restart radiod automatically**.
- If the shipped config example changed, apt hits a conffile prompt;
  add `-o Dpkg::Options::="--force-confold"` to keep the local config.
- Optional provenance check (any machine with `gh`):
  `gh attestation verify radiod_….deb --repo st3fan/radio`.
- Health: `journalctl -u radiod -n 5` shows `radiod X.Y.Z listening`,
  the mixer-ceiling line and `advertising as "Radio"`; the site answers
  on port 80.

## Autopilot

When Stefan asks Claude to do a release, that is standing permission to
run **every step above autonomously**: agree on the version number
first, then Claude does the rest — the bump PR (including merging it),
publishing the release, watching the workflow, and the verify-and-
upgrade on the radio. Claude-run releases end the release description
with an attribution line:

> *Release done by Claude with permission of @st3fan.*

## Prereleases (testing the workflow itself)

Tag `vX.Y.Z-rcN` with `--prerelease`, optionally `--target <branch>`;
the version check is warn-only for prereleases. Delete release and tag
afterwards (`gh release delete vX.Y.Z-rcN --cleanup-tag --yes`).

## Gotchas learned the hard way

- Same-version dev redeploys are **not** upgrades: apt silently reuses
  its cached deb — use `dpkg -i` for those (see
  `notes/clean-install.md`).
- The website's old standalone deb era is gone: since 0.3.x radiod
  Conflicts/Replaces `radio-website`.
