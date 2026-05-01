# Security Policy

How to report vulnerabilities in udoc, the threat model the project is
designed against, and the hardening posture maintained on the trunk.

## Reporting a vulnerability

Private disclosure is preferred for any issue that could affect downstream
consumers.

- **Preferred channel.** [GitHub Security Advisory](https://github.com/newelh/udoc/security/advisories/new)
  on this repository (Security tab, *Report a vulnerability*). Creates a
  private advisory only the maintainers can see and lets us collaborate on
  a fix before public disclosure.
- **Fallback channel.** Email me@newel.dev. Use this if you cannot file a
  GHSA for any reason.

Please include, when possible:

- A minimal reproduction. A corpus seed under ~5 MB if the bug triggers
  via malformed input.
- The udoc version (commit SHA or published version).
- Your CVSS v3.1 vector if you have one; we will score during triage
  otherwise.

This is an alpha release maintained at low headcount. We do not commit to a
response SLA. Reports are read; fixes are prioritised by severity.

## Threat model

### In scope

- **Denial of service via malformed input.** Allocation bombs (attacker-
  controlled size fields driving unbounded allocations), decode-loop
  hangs, peak-RSS exhaustion.
- **Information leak via out-of-bounds reads.** Parser bounds-check
  failures that could read past the end of an input buffer or surface
  uninitialised memory.
- **Data corruption.** Integer-truncation or aliasing bugs that cause one
  input element to be silently swapped with another.
- **Supply-chain risk via direct dependencies.** Direct deps are pinned
  and `cargo audit` runs on every push. Any active RUSTSEC advisory
  against a shipped dependency is treated as a security finding.

### Out of scope

- **Hooks executing user-provided binaries.** The `--ocr` / `--hook`
  CLI flags and the equivalent library API explicitly run external
  programs over the JSONL hook protocol. The I/O channel is bounded
  (one request per line, configurable timeout), but the hook process
  itself is not sandboxed. Trust the binaries you wire in.
- **Bit-for-bit reproducibility of any quality metric across hosts.**
  Performance, SSIM, and accuracy figures vary with hardware,
  operating system, and corpus. They are not security properties.

## Hardening posture

- **Workspace-wide `#![deny(unsafe_code)]`** at every crate's `lib.rs`.
  One isolated, audited `unsafe` block in `udoc-pdf::io::mmap_impl`
  for memory-mapped file access. See `docs/unsafe.md` for the full
  audit.
- **Continuous fuzzing.** A suite of fuzz targets runs nightly across
  the parser surface (PDF objects, streams, content; ZIP, XML, CFB,
  RTF; font tables; image decoders). Crashes file issues automatically;
  regression seeds for every fixed finding replay on every PR.
- **HashDoS-resistant hashing** (`ahash`) on every map keyed by
  attacker-controlled values. `FxHash` is reserved for maps keyed by
  integers the codebase generates itself.
- **Resource budgets.** `Config::limits` bounds per-document resource
  use (max file size, max page count, etc.). `Config::memory_budget`
  provides an opt-in soft RSS cap for long-running batch workers,
  triggering between-document cache resets when crossed.
- **Crypto via RustCrypto** (`md-5`, `aes`, `cbc`), isolated to the PDF
  encryption module. No homegrown cryptographic primitives.

## CVSS triage

Every finding is scored with a CVSS v3.1 vector recorded in the fixing PR
or the security advisory. Fixes for findings scoring CVSS 5.0 or higher
block the next release; lower-severity issues route to the next regular
release.

A worked example for a typical "attacker sends a malformed file, parser
hangs" finding:

```
CVSS:3.1/AV:L/AC:L/PR:N/UI:R/S:U/C:N/I:N/A:H  (Score: 6.1 Medium)
```

Where:

- **AV** (Attack Vector): `L` (local) for "attacker sends us a file"; `N`
  (network) only if our code is the server.
- **AC** (Attack Complexity): `L` if trivial repro; `H` if specific
  conditions are required.
- **PR** (Privileges Required): `N` for untrusted input.
- **UI** (User Interaction): `R` if a user opens a file; `N` if ingest is
  automated.
- **S** (Scope): `U` unchanged (our process); `C` changed (affects other
  processes).
- **C/I/A** (Confidentiality / Integrity / Availability): DoS = `A:H`,
  info leak = `C:H`, data corruption = `I:H`.

## Public disclosure

Coordinated with the reporter once a fix has shipped, or 90 days after
triage if no fix is available, whichever comes first. Reporters who
prefer a different timeline should say so in the initial report.

Past advisories live on the
[Security advisories](https://github.com/newelh/udoc/security/advisories)
page once disclosed.
