# Reverse Beacon Network pipeline

Sidecar connects to the RBN Telnet endpoint only when `--rbn` is explicitly enabled and
a login callsign is supplied. Its default startup opens no cluster connection and is
safe for an unassisted Single Operator entry. The RBN documents port 7000 as a combined CW and RTTY feed, so
Sidecar accepts only well-formed DX lines carrying a numeric `WPM` observation followed
by an explicit `CQ` activity marker. Beacon, DX, non-CW, missing-activity, and ambiguous
records are not candidates. Other cluster output is ignored and is visible only with
debug logging.

Candidates are bounded by configurable age, call/band/time/frequency deduplication,
and total capacity. Aggregated candidates retain the newest observation, recent and
best SNR, total reports, and the distinct reporting skimmers. Disconnects keep the
last-known candidates but mark them stale until each one is observed again.

Reconnect delay uses capped exponential backoff with per-process ±20% jitter. The
dashboard does not report a live connection merely because the TCP socket opened: it
waits for a positive login acknowledgement containing the configured callsign or the
first valid spot. A stalled login times out after 15 seconds, and rejected or dropped
sessions reconnect normally. The backoff resets after an established connection. Timing, expiry, aggregation,
classification, malformed input, and representative high-volume behavior are covered
by deterministic tests.

## Optional live smoke test

Automated tests use only a local fake cluster. To manually verify the public endpoint,
start Sidecar outside an active unassisted entry with a valid callsign:

```bash
QSO_SIDECAR_CALL=N1RWJ mise run start -- --rbn
```

Open the dashboard and confirm that the RBN status progresses from connecting to live
only after the server acknowledges the login or emits a valid CQ spot. Stop Sidecar,
repeat once to exercise a fresh session, and do not attach captured live output to bug
reports because it may include operator callsigns.

## Classification evidence policy

Sidecar does not ship a callsign-history database and does not infer NAQP participation,
location, or multiplier credit from an arbitrary RBN callsign. A candidate is shown as
a verified multiplier only when a prior QSO in the operator's local log supplies a
resolvable exchange; without that evidence it remains unknown.
The official [NCJ results search](https://ncjweb.com/naqpscores/) contains historical
submitted score/QTH data, not a versioned current call-history artifact with an
identified reuse license. [NCJ has also cautioned](https://ncjweb.com/naqp-scores/ssbnaqp082022.pdf)
that call-history and callbook lookup can produce high exchange error rates. Without a
current, provenance-preserving, legally reusable source, adding automatic exchange
inference would be misleading.

If a suitable source is added later, every inferred value must carry source/version
provenance, remain visibly tentative, and never affect the claimed score.
