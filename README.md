# resume-parser

Multi-agent, evidence-grounded resume → **trusted** Excel extractor. Point it at a
folder of resumes (PDF / DOCX / DOC / RTF / TXT) and get one spreadsheet row per
candidate with the fields IT recruiters actually filter on:

> Name · WhatsApp/Phone · Email · Currently Working At · Currently Employed? ·
> Gap? + duration · Years of Experience · Fresher/Experienced · Primary IT Role

Powered by **Gemini Flash Lite** for the reasoning-heavy fields, with a deterministic
Rust core that verifies everything against the source document.

## Why you can trust the output

Fully-automated 100% accuracy is impossible on arbitrary resumes (some don't contain
the data; some fields have no single correct answer). So instead of hiding errors,
this tool makes every possible error **visible and cheap to catch**:

- **Specialist agents.** Contact, Employment-History, Role, Skills and a Verifier each
  focus on one job, which is more robust to the many ways resumes are written.
- **Evidence for every field.** Each value carries the verbatim quote it came from, and
  the code checks that quote is actually in the resume → hallucinations can't survive.
- **Code does the math, not the LLM.** The model only *finds* dates; Rust computes
  years-of-experience and gaps deterministically. A concrete end date always overrides
  a mistaken "still employed" flag.
- **2-pass consensus + Verifier.** Employment and Role are extracted twice and compared;
  a Verifier agent adversarially audits the result. Disagreements are flagged, not
  averaged.
- **Confidence gate.** Every row is ✅ *auto-verified* or ⚠️ *needs review*, with the
  weak cells colour-coded and their evidence attached as Excel notes. You review only
  the flagged minority — that's the path to a sheet you can trust completely.

## Setup

macOS or Linux + a Gemini API key. One command installs the Rust toolchain, OS
build deps, compiles the binary, and seeds a `.env`:

```bash
git clone git@github.com:ayush-jain-mechademy/resume-parser.git
cd resume-parser
./setup.sh
# then put your key in .env  ->  GEMINI_API_KEY=...
```

👉 **New here? Read the [User Guide](./GUIDE.md)** — install, every command, reading
the metrics, understanding the review flags, and debugging.

## Usage

```bash
./run.sh /path/to/resumes                       # → candidates.xlsx (+ .csv + .json + .metrics.json)
# or directly:
./target/release/resume-parser /path/to/resumes -o candidates.xlsx
```

Options:

| Flag | Default | Meaning |
|---|---|---|
| `-o, --output` | `candidates.xlsx` | Output path (`.csv`/`.json` siblings written too) |
| `--model` | `gemini-2.5-flash-lite` | Gemini model for the specialist agents |
| `--workers` | `6` | Resumes processed concurrently |
| `--max-concurrent` | `5` | Global cap on simultaneous API calls — **lower if you hit rate limits** |
| `--store` | `<output dir>/resume_store.sqlite` | Persistent cache + dedupe + audit trail |
| `--no-cache` | off | Re-extract every file, ignoring the cache |
| `--limit N` | — | Only process the first N files (quick trial) |

The **store** is a SQLite database: unchanged files are skipped on re-runs, the same
candidate across two files is de-duplicated (by email/phone), and every extraction is
kept for audit and for the review step.

## Reviewing the flagged rows

Rows the confidence gate isn't sure about are marked **⚠️ Review** and sorted to the
top of the sheet. For each, the uncertain cells are colour-coded (red = low, amber =
medium) and the **verbatim evidence** the value came from is attached as an Excel cell
note (hover to read). Confirm or correct those cells — everything marked **✅ Verified**
was high-confidence and internally consistent, so you can trust it as-is.

## Re-running is safe (idempotent)

- **Cache:** a re-run over the same folder skips every unchanged file (matched by
  content hash) and produces **byte-identical** output — no duplicate work, no drift.
- **Resumable:** each candidate is written to the store the moment it finishes, so an
  interrupted run just continues from where it stopped.
- **Atomic writes:** the `.xlsx`/`.csv`/`.json` are written to a temp file and renamed,
  so an interrupted run never leaves a half-written or corrupt file.
- **Corrections stick:** a human-verified record is never overwritten by a later
  re-extraction.

## Robustness

Built to never crash on a bad input: each resume is parsed in an isolated task, so a
malformed PDF (even one that panics the PDF library) is contained and falls back to
Gemini's native-PDF vision rather than taking down the batch. Contact info, role, and
name all have deterministic fallbacks if an agent call fails.

## ⚠️ Privacy

Resumes are candidate PII. On Google's **free** Gemini tier, prompts may be used to
improve their products and be seen by human reviewers. **Enable billing** on the Google
project (paid tier does not train on your data) before running on real candidate data.
This tool reads the key only from the environment — it is never stored in code.

## Rules (configurable in `src/config.rs`)

- **Years of experience** = summed duration of full-time/contract/freelance roles
  (overlaps merged, internships/education excluded).
- **Fresher** = has never held a full-time role (fresh grad or interns-only).
- **Gap** = months since the last job ended *if not currently employed*; flagged at ≥ 2
  months. Mid-career gaps are noted, not headlined.
- **Primary role** ∈ Full Stack · Frontend · Backend · Data Analyst · Data Scientist/ML ·
  AI/ML Engineer · Data Engineer · DevOps/Cloud · Mobile · QA/Testing · UI/UX ·
  Business/Systems Analyst · IT Support · Other.

## Metrics (KPIs)

Every run prints and writes (`<output>.metrics.json`) both **engineering** and **product**
KPIs:

- **Engineering**: files discovered, cache-hit rate, parse-success rate, failures/timeouts,
  throughput (resumes/min), avg latency, Gemini calls, tokens, cost (total + per resume),
  vision-fallback count.
- **Product**: candidate count, auto-verify vs review rate, field coverage
  (email/phone/name/role %), confidence distribution, experienced-vs-fresher split,
  employed/gap counts, and role distribution.

## Tests

```bash
cargo test          # temporal math, contact extraction, the is_current guard
```
