# Beta — install it offline, break it, report it

This build is the Phase-7 output: an installer for **Windows 10/11 x64** that
carries everything it needs, including Microsoft's WebView2 runtime. The point of
the beta is §8's exit criterion — *a USB install with networking disabled works* —
and that criterion is not something a test suite can produce. It needs a person,
a machine, and the network switched off.

## 1. Get the build

The installer is a CI artifact, not a release download: it is built on demand and
kept for 30 days.

* **From the Actions UI**: open the latest run of the **CI** workflow on the
  `arena/01a0b0c7-icon-forge` branch (its `Installer` job), or the latest
  **Release** workflow run, and download the artifact `icon-forge-windows-nsis`.
* **From the command line**: `gh run download <run-id> -n icon-forge-windows-nsis`
  (needs a logged-in `gh`).
* **Build a fresh one**: push a tag matching `v0.0.0-verify*`
  (`git tag -a v0.0.0-verify-beta -m "beta build" && git push origin v0.0.0-verify-beta`).
  The verification-tag pattern exists because a workflow that lives only on an
  unmerged branch cannot be dispatched from the UI.

The run's own notice carries the exact file name, byte size, SHA-256 and the
WebView2 payload listing — read it with:

```bash
gh api /repos/mraaisa-afk/icon-forge/check-runs/<job-id>/annotations \
  --jq '.[] | select(.title | test("installer")) | .message'
```

## 2. Verify what you downloaded

```powershell
certutil -hashfile "Icon Forge_0.1.0_x64-setup.exe" SHA256
```

Compare with the `P2` line from the run's notice. **Only compare against the run
you downloaded from**: builds are not byte-reproducible (three builds of one
commit differed), so the hash identifies *that file*, not the source revision.

## 3. The install test (±20 minutes)

Do it on a machine that is not yours, if you have one, and **switch the network
off first** — cable out or airplane mode, before setup runs. That ordering is the
whole test: the installer must not reach for anything.

1. **Install.** Run the setup executable. It installs per-user, so expect **no
   admin prompt**. Windows SmartScreen will warn because the installer is
   unsigned: *More info → Run anyway*. Note whether anything else prompted you.
2. **Watch for network.** If setup or first launch asks for a download, that is a
   failure — record the exact message and stop.
3. **Launch** and import a sheet from `bench/corpus/` — `12_c2_latency_grid.png`
   (100 icons, 4096²) is the interesting one. Then run **Group All**.
4. **Vectorize** the sheet, then open **Review** and triage ~20 icons with
   `A`/`R`/`F`/`D`, `Space` for the overlay, `Ctrl+Z` to undo one.
5. **Export** the sheet (CSV plus at least one image format) and open the export
   in another tool (Explorer preview, Inkscape, a browser) to confirm it is valid.
6. **Save the project**, close the app, reopen it, and confirm the library is
   still there.

### Record

| What | Where it comes from |
|---|---|
| Pass/fail for each step above | your notes |
| Install time, first-launch time | a clock |
| Group All time on the 100-icon sheet | the app's status line (target: well under 2 s) |
| Icons triaged per minute in Review | the workspace's own pace line |
| Anything that asked for the network | exact wording |
| Anything that looked wrong, and what you expected | screenshots help |

## 4. Known limits — these are not bugs to report

* **Unsigned installer.** SmartScreen will warn. Code signing is an open Phase-7
  item, not an oversight.
* **Builds are not byte-reproducible.** Hashes differ between builds of the same
  source.
* **No Content-Security-Policy is set yet** (`csp: null`). Recorded, not hidden.
* **Non-goals** (§0): cloud sync, collaboration, mobile, non-Latin glyph tracing.
* **One sheet per review pass.** Bulk actions apply to the open sheet.

## 5. The score we need

NPS is one question — *"How likely are you to recommend Icon Forge to a colleague
who does icon work, 0–10?"* — plus the reason for the number, in your words.
Record it with the install result above; §8's bar is **≥ 40**.

## 6. Where the results go

Reply in the thread that produced this build (or open an issue with the `beta`
label), pasting the table from §3 and the NPS answer. Include the run id you
downloaded from so the exact artifact can be identified.
