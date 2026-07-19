# resume-parser — User Guide

A practical, end-to-end guide: install, run, read the results, understand the
metrics, and debug. For the *why* behind the design (how it stays accurate), see
[README.md](./README.md).

---

## 1. Getting started

### Prerequisites
- **macOS**, or **Linux** — Ubuntu 18.04+ / Debian / Amazon Linux 2 & 2023 /
  Fedora / Arch (x86-64 or arm64). `setup.sh` installs the toolchain + build deps
  for apt/dnf/yum/pacman automatically.
- A Gemini API key — get one free at <https://aistudio.google.com/apikey>
  (⚠️ enable billing before running real candidate data — see [Privacy](#8-privacy))

> On Linux, legacy `.doc`/`.rtf` files are skipped (they rely on macOS `textutil`);
> **PDF, DOCX and TXT work on every platform.**

### Install (one command)
```bash
git clone git@github.com:ayush-jain-mechademy/resume-parser.git
cd resume-parser
./setup.sh
```
`setup.sh` installs the Rust toolchain if missing, installs the OS build
dependencies, compiles the optimized binary, and creates a `.env` for you.

### Add your key
```bash
# edit .env and set:
GEMINI_API_KEY=your-key-here
# — or export it in your shell:
export GEMINI_API_KEY=your-key-here
```

### First run
```bash
./run.sh sample_resumes        # bundled synthetic resumes — safe to test with
```
You should get `candidates.xlsx` plus `.csv`, `.json`, and `.metrics.json`.

---

## 2. What it does

Point it at a folder of resumes (**PDF / DOCX / DOC / RTF / TXT**, searched
recursively) and it produces one spreadsheet row per candidate:

| Column | Meaning |
|---|---|
| Status | ✅ Verified (trust as-is) or ⚠️ Review (needs a human glance) |
| Name · WhatsApp/Phone · Email | Contact details (phone normalized to E.164) |
| Currently Working At | Current employer, or `—` if not employed |
| Currently Employed? · Gap? · Gap Duration | Employment status + current gap |
| Years of Experience · Fresher/Experienced | Computed from the work-history dates |
| Primary Role · Secondary Role | Best-fit IT role bucket |
| Key Skills · Confidence · Review Reason | Skills, overall confidence, why it's flagged |

---

## 3. Running commands

```bash
# basic
./target/release/resume-parser <folder> -o candidates.xlsx

# via the wrapper (builds release + runs)
./run.sh <folder> [extra flags]
```

### All flags
| Flag | Default | What it does |
|---|---|---|
| `-o, --output <path>` | `candidates.xlsx` | Output file; `.csv`/`.json`/`.metrics.json` written alongside |
| `--model <name>` | `gemini-2.5-flash-lite` | Gemini model for the specialist agents |
| `--workers <n>` | `6` | Resumes processed concurrently |
| `--max-concurrent <n>` | `5` | Global cap on simultaneous API calls — **lower if you hit rate limits** |
| `--timeout <secs>` | `150` | Hard per-resume cap; a file that exceeds it is abandoned (never stalls the batch) |
| `--store <path>` | `<output dir>/resume_store.sqlite` | Cache + dedupe + audit database |
| `--no-cache` | off | Re-extract every file, ignoring the cache |
| `--limit <n>` | — | Only process the first N files (quick trial) |

### Common examples
```bash
# quick trial on 3 files
./target/release/resume-parser ./resumes --limit 3

# a big batch, gentler on free-tier rate limits
./target/release/resume-parser ./resumes --workers 3 --max-concurrent 3

# force a clean re-run (ignore cache)
./target/release/resume-parser ./resumes --no-cache

# use the newest model
./target/release/resume-parser ./resumes --model gemini-3.1-flash-lite
```

---

## 4. Reading the output

Four files are written next to your `-o` path:

- **`candidates.xlsx`** — the main deliverable. ⚠️ rows are sorted to the **top**;
  low-confidence cells are colour-coded (**red** = low, **amber** = medium); the
  **evidence** each value came from is attached as an Excel **cell note** (hover the
  Name / Currently-Working-At / Primary-Role cells).
- **`candidates.csv`** — same rows, plain CSV.
- **`candidates.json`** — full structured records incl. parsed job history + evidence.
- **`candidates.metrics.json`** — the KPIs (see next section).

### The status column
- **✅ Verified** — every field was high-confidence and internally consistent.
  Safe to use directly.
- **⚠️ Review** — at least one field was uncertain, the two model passes
  disagreed, or a sanity rule fired. The `Review Reason` column says exactly why.
  You only need to look at these.

---

## 5. Stats & metrics (KPIs)

Every run prints two KPI panels and writes them to `<output>.metrics.json`.

```
╭─ Engineering KPIs ─────────────────────────
│ Files discovered      19
│ Cache hit rate        0%  (0 cached, 19 extracted)
│ Parse success rate    94.7%  (18/19 attempted)
│ Failures / timeouts   1 / 1
│ Throughput            10.6 resumes/min
│ Avg latency/resume    5.6s
│ Wall-clock            107.1s
│ Gemini API calls      131  (6.9/resume)
│ Tokens                167300 in + 24250 out = 191550
│ Cost                  $0.0264  ($0.00139/resume)
│ Vision fallbacks      0
╰────────────────────────────────────────────
╭─ Product KPIs ─────────────────────────────
│ Candidates            18
│ Auto-verified         67%  (12/18)   Review 33%  (6)
│ Field coverage        email 100% · phone 100% · both 100% · name 100% · role 100%
│ Confidence            High:12  Low:3  Medium:3
│ Experience mix        16 experienced · 2 fresher
│ Employment            13 employed · 5 not · 3 with current gap
│ Top roles             AI/ML Engineer:7  Full Stack:4  Backend:2  Data Analyst:2 …
╰────────────────────────────────────────────
```

### View / query the metrics later
```bash
cat candidates.metrics.json | python3 -m json.tool      # pretty-print
# specific KPIs with jq:
jq '.engineering.cost_usd'            candidates.metrics.json
jq '.product.auto_verify_rate_pct'    candidates.metrics.json
jq '.product.role_distribution'       candidates.metrics.json
```

**Engineering KPIs**: files discovered, cache-hit rate, parse-success rate,
failures/timeouts, throughput, avg latency, Gemini calls, tokens, cost (total +
per resume), vision-fallback count.
**Product KPIs**: candidate count, auto-verify vs review rate, field coverage %,
confidence distribution, experienced-vs-fresher, employed/gap counts, role
distribution.

---

## 6. The store (cache, dedupe, idempotency)

The `--store` SQLite DB makes re-runs cheap and safe:
- **Cache** — an unchanged file (matched by content hash) is skipped on re-run;
  output is byte-identical.
- **Dedupe** — the same candidate across two files (matched by email/phone) is
  collapsed to one row.
- **Resumable** — each candidate is saved the moment it finishes, so an
  interrupted run continues where it stopped.
- **Corrections stick** — a human-verified record is never overwritten by a later
  re-extraction.

Inspect it directly:
```bash
sqlite3 resume_store.sqlite "SELECT COUNT(*) FROM candidates;"
sqlite3 resume_store.sqlite "SELECT json FROM candidates;" | python3 -m json.tool
```

---

## 7. Debugging & troubleshooting

| Symptom | Fix |
|---|---|
| `GEMINI_API_KEY … is not set` | `export GEMINI_API_KEY=...` or put it in `.env` |
| `Gemini HTTP 429` / very slow | Free-tier rate limit — add `--max-concurrent 3 --workers 3`, or enable billing |
| A file shows `timed out after Ns` | That one PDF was pathological and abandoned (batch still completes); re-save the PDF and re-run, or raise `--timeout` |
| `no supported resumes found` | Check the folder path and that files are pdf/docx/doc/rtf/txt |
| A candidate looks wrong | Check its `Review Reason`; open the source PDF; hover the evidence note in the Excel |
| Want to re-extract one changed file | Just re-run — the hash cache re-processes only changed files (or `--no-cache` for all) |
| Legacy `.doc`/`.rtf` fails on Linux | Those use macOS `textutil`; convert to PDF/DOCX, or run on macOS |

### Quick diagnostics
```bash
# smoke-test the Gemini connection + structured output
cargo run --example gemini_smoke

# run the deterministic unit tests (date math, contact extraction, is_current guard)
cargo test

# trial on a tiny subset before a big batch
./target/release/resume-parser ./resumes --limit 3 --no-cache
```

### Adjusting the rules
The extraction rules live in `src/config.rs` (`Settings` + `Role`):
- `gap_flag_months` — min months to flag a current gap (default 2)
- `min_text_chars` — below this a PDF is treated as scanned → vision fallback
- role buckets + keyword profiles — edit `Role` to add/rename roles
Rebuild after changes: `cargo build --release`.

---

## 8. Privacy

Resumes are candidate PII. On Google's **free** Gemini tier, prompts may be used
to improve their products and be seen by human reviewers. **Enable billing** on
the Google project (the paid tier does not train on your data) before running
real candidate resumes. The tool reads the key only from the environment and
never stores it. Extracted data (`*.xlsx/csv/json`, the SQLite store) and any
`real_resumes/` folder are git-ignored so they're never committed.
