# QSO Sidecar

A local, read-only contest dashboard for Ham2K PoLo. The contest-day MVP scores the
August 2026 NAQP CW, shows the per-band multiplier matrix, accepts current PoLo ADIF
exports, polls Ham2K Log Filer (LoFi), and can display live Reverse Beacon Network CW
candidates. PoLo remains the authoritative logger: Sidecar has no QSO entry or edit UI.

Everything is served by one stable-Rust binary on loopback. HTML, CSS, and JavaScript
are embedded in the release binary; there is no frontend toolchain or internet-loaded UI.

## Fast start

Install [mise](https://mise.jdx.dev/) and then:

```bash
mise run build
mise run start
```

Open <http://127.0.0.1:7878>. To exercise the entire UI without showing a real log:

```bash
mise run start -- --demo --no-rbn
```

Development and verification tasks:

```bash
mise run dev -- --demo --no-rbn
mise run check
mise run build
```

## Connect to PoLo through LoFi

1. Start Sidecar and open <http://127.0.0.1:7878>.
2. In **PoLo connection**, enter the email address attached to the Ham2K account and
   choose **Send link**.
3. Open the verification email and follow its Ham2K link. Leave Sidecar running; it
   polls the linked-account status every eight seconds.
4. Once linked, Sidecar fetches recent operations and automatically prefers the current
   operation containing a `naqp` reference. If needed, choose another operation from
   the dropdown.
5. Sidecar fetches all pages of that operation's QSOs, then polls incrementally every
   eight seconds. Updates and tombstones replace prior records by QSON UUID.

The random LoFi client key, secret, and bearer token are stored only in the OS per-user
application-data directory. The directory is mode `0700` and the credential file is
mode `0600` on Unix. They are never sent to browser JavaScript or written to logs.

## Guaranteed ADIF fallback

In PoLo, export the current operation as ADIF. Drag the `.adi`/`.adif` file onto the
dashboard's import target, or focus it and press Enter to select a file. The parser uses
`CALL`, `QSO_DATE`, `TIME_ON`, `BAND`/`FREQ`, `MODE`, `NAME`, `STATE`, `COUNTRY`, `DXCC`,
`CONTEST_ID`, and `SRX_STRING`. PoLo's `SRX_STRING` form (`<name> <location>`) is used as
an exchange fallback. Re-importing a growing snapshot is idempotent.

## Live CW spots and entry category

By default Sidecar connects to `telnet.reversebeacon.net:7000`. Set the login callsign:

```bash
QSO_SIDECAR_CALL=N1RWJ mise run start
```

**NAQP prohibits spotting/skimmer assistance for the Single Operator category.** Using
the live candidate panel means entering Single Operator Assisted. To keep Sidecar safe
for an unassisted entry, start it with `--no-rbn`; the dashboard also shows a persistent
warning whenever spots are enabled.

Spots never award QSO or multiplier credit. An exchange copied from a worked QSO on
another band is labeled **verified needed**; inference is explicitly labeled possible.

## Configuration

CLI flags also have environment-variable equivalents:

| Flag | Environment variable | Default |
| --- | --- | --- |
| `--port` | `QSO_SIDECAR_PORT` | `7878` |
| `--cluster` | `QSO_SIDECAR_CLUSTER` | `telnet.reversebeacon.net:7000` |
| `--call` | `QSO_SIDECAR_CALL` | unset |
| `--no-rbn` | `QSO_SIDECAR_NO_RBN` | false |
| `--demo` | `QSO_SIDECAR_DEMO` | false |
| `--lofi-base` | `QSO_SIDECAR_LOFI_BASE` | `https://lofi.ham2k.net` |

The HTTP listener always binds to `127.0.0.1`. Use `RUST_LOG=qso_sidecar=debug` for
additional diagnostics; credentials and QSO payloads are not logged.

## Scoring scope

The engine implements the [2026 NAQP rules](https://ncjweb.com/NAQP-Rules.pdf): CW-only
contacts from 18:00 UTC August 1 through 05:59:59 UTC August 2, eligible 160/80/40/20/15/10
meter bands, one callsign per band, per-band multipliers, explicit duplicate/unresolved
tracking, and claimed score = valid QSOs × per-band multipliers. Estimated off time uses
the required 31-minute consecutive-QSO timestamp boundary. Adjudication penalties are
outside this claimed-score companion.
