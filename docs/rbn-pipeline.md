# Reverse Beacon Network pipeline

Sidecar connects to the RBN Telnet endpoint only when spots are enabled and a login
callsign is supplied. The RBN documents port 7000 as a combined CW and RTTY feed, so
Sidecar accepts only well-formed DX lines carrying a numeric `WPM` observation. Other
cluster output is ignored and is visible only with debug logging.

Candidates are bounded by configurable age, call/band/time/frequency deduplication,
and total capacity. Aggregated candidates retain the newest observation, recent and
best SNR, total reports, and the distinct reporting skimmers. Disconnects keep the
last-known candidates but mark them stale until each one is observed again.

Reconnect delay uses capped exponential backoff with per-process ±20% jitter. The
backoff resets after an established connection. Timing, expiry, aggregation,
classification, malformed input, and representative high-volume behavior are covered
by deterministic tests.

## Exchange inference policy

Sidecar does not ship a callsign-history database and does not infer multiplier credit.
The official [NCJ results search](https://ncjweb.com/naqpscores/) contains historical
submitted score/QTH data, not a versioned current call-history artifact with an
identified reuse license. [NCJ has also cautioned](https://ncjweb.com/naqp-scores/ssbnaqp082022.pdf)
that call-history and callbook lookup can produce high exchange error rates. Without a
current, provenance-preserving, legally reusable source, adding automatic exchange
inference would be misleading.

If a suitable source is added later, every inferred value must carry source/version
provenance, remain visibly tentative, and never affect the claimed score.
