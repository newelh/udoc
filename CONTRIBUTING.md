# Contributing to udoc

## Issues are welcome

If you have found a bug, hit a format we don't handle well, want to
report a quirky document that breaks extraction, or have an ergonomic
suggestion, please open an issue. The issue templates under
`.github/ISSUE_TEMPLATE/` cover the common cases (bug, feature,
format-support gap, question). If none fit, the blank template is fine.

A bug report is most useful when it includes:

- The udoc version you are running (`udoc --version` or
  `udoc.__version__` from Python).
- The format and approximate size of the document. Where possible, a
  minimal reproduction (a tiny file or a published one we can fetch).
- The output you expected and the output you got.

For documents you cannot share publicly, file an issue describing the
shape of the problem and offer to share the file privately. We will
arrange a private channel.

## Pull requests are not accepted at this time

This repository does not accept pull requests during the alpha period.
udoc is solo-maintained and the codebase is moving fast; merging
external changes against a moving target costs more than it saves right
now. Pull requests opened against this repository will be politely
declined.

The right path for a change is:

1. Open an issue describing what you would like to see.
2. If the maintainer agrees the change is in scope, they will land it.
3. You will be credited in the issue and the release notes.

Pull requests will reopen at the beta milestone. The bar at that point
will still be "discuss first," but the door will not be closed.

## Reporting a vulnerability

Do not file vulnerabilities as public issues. See
[SECURITY.md](SECURITY.md) for the disclosure process. The short
version is: GitHub Security Advisories are preferred, and
me@newel.dev is the email fallback.
