# Security policy

## Supported versions

The latest release is supported. Older releases, including the last Bun release
(`v0.1.0`), receive fixes only at the maintainer's discretion.

## Report a vulnerability privately

Use GitHub's private security advisories for this repository:

<https://github.com/RantAI-dev/rantai-kb/security/advisories/new>

Do **not** open a public issue, pull request or discussion for a suspected
vulnerability. Include:

- the version or commit you tested, and how it was deployed (tier, compose, bare binary);
- the request or sequence that reproduces it;
- what you expected and what happened;
- whether data from another tenant or knowledge base was exposed.

We will acknowledge the report in the advisory thread, investigate, and coordinate a
fix and disclosure with you.

## What counts as a security issue

- **Tenant scope leaks.** Any route or retrieval arm that returns a document, chunk,
  figure, page image, job or event belonging to another tenant.
- **Knowledge-base scope leaks.** A key limited with `--kb` reading or changing a
  document outside its knowledge bases. The known by-ID gap is tracked as `SEC-1` in
  `docs/core/BACKLOG.md`; report it if you find a path that is not covered there.
- **Authentication or authorisation bypass.** Using no key, a revoked key, or a key
  with the wrong scope to reach a protected route.
- **Secret exposure.** API keys, embedding keys or store credentials leaked through a
  response, log, error or image.

Ordinary retrieval-quality issues and missing features (users, roles, folders,
per-document permissions) are not security reports; they belong in the backlog.
