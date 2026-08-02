# Plan: build provenance attestations for release artifacts

- **Date:** 2026-08-01
- **Status:** draft

## Background

Release .debs are distributed as GitHub Release assets with a
`SHA256SUMS` file. Checksums prove *what* the bytes are, but nothing
proves *where they came from* — that a given deb was built by this
repository's release workflow from a specific commit, rather than on
someone's laptop or by a tampered pipeline.

GitHub's [artifact attestations](https://github.com/actions/attest-build-provenance)
close that gap: the workflow generates a **signed SLSA build-provenance
attestation** per artifact, bound to the workflow's OIDC identity and
recorded in GitHub's Sigstore instance. Anyone can verify a download:

```
gh attestation verify radiod_<v>_arm64.deb --repo st3fan/radio
```

Free for public repositories. This is the modern replacement for the
deb-signing question the release-pipeline research already answered
("nobody GPG-signs GitHub-release debs; checksums are the norm"):
checksums stay for integrity, attestations add provenance.

## Design

All changes live in `.github/workflows/release.yml`:

- **Permissions**, per job: the `radiod` matrix job gets
  `contents: read`, `id-token: write`, `attestations: write` (it never
  writes repo contents); the `release-assets` job adds `id-token` and
  `attestations` to its existing `contents: write`.
- **One attest step per artifact**: in each matrix leg, after `Build
  .deb`, `actions/attest-build-provenance@v3` with `subject-path` set to
  the built deb; in the fan-in job, the same for the website deb. Four
  attested debs per release; `SHA256SUMS` itself is not attested (it is
  derived from the attested artifacts).
- The action is a JS action, so it runs inside the `debian:trixie`
  containers the same way checkout and upload-artifact already do.
- **Docs**: README's release section gains the one-line verify command.

Scope notes:

- Attestations apply to releases built after this lands; `v0.2.0`
  remains checksum-only (re-tagging it is not worth the churn).
- Consumers verify with the `gh` CLI; apt does not check attestations —
  this is supply-chain tooling, not install-time enforcement.

## Phases

One stack: this plan as the bottom PR, one implementation PR on top.

### Phase 1 — attest steps + docs

The workflow changes and the README verify note. Verification: publish
a throwaway prerelease (`v0.0.0-rc4`, prerelease-tolerant version
check) targeting the implementation branch; confirm every leg's attest
step succeeds, then `gh attestation verify` a downloaded deb against
`st3fan/radio` and see it pass. Delete the prerelease afterwards.

## Acceptance criteria

- A release run attests all four .debs (three radiod arches + website).
- `gh attestation verify <deb> --repo st3fan/radio` succeeds for a
  downloaded asset.
- The attestations are visible under the repository's Attestations
  view on GitHub.
- README documents the verify command.
