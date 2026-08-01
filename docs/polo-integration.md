# PoLo integration and privacy boundary

QSO Sidecar is a read-only companion. Ham2K PoLo remains the authoritative logger,
and Sidecar never creates, edits, or uploads a QSO.

## Supported sources

Sidecar accepts two normalized log-source updates:

- A PoLo ADIF export is an incremental snapshot. Re-importing a growing export is
  idempotent, and record-level parse problems are reported without discarding the
  previous last-good state.
- Ham2K Log Filer (LoFi) supplies an initial full operation snapshot followed by
  incremental updates. QSON UUIDs are the stable identity; updates and tombstones
  replace the prior UUID state.

The LoFi protocol was derived from the public web client and remains less stable
than a published versioned API. Sidecar therefore preserves unknown QSON fields,
keeps the ADIF path as the guaranteed fallback, and treats all failed network or
decode operations as non-destructive.

## Durable last-good state

After a successful ADIF import or LoFi update, Sidecar atomically replaces
`last-good-state.json` in the operating system's per-user application-data
directory. The snapshot contains normalized QSOs (including tombstones), selected
operation, source kind and label, refresh time, and import diagnostics. It does not
contain the LoFi key, secret, bearer token, email address, or RBN data.

On Unix the application-data directory is mode `0700` and both the credential file
and last-good snapshot are mode `0600`. The snapshot is written and synced to a
temporary file before rename, so an interrupted or failed write leaves the previous
state intact. Startup restores that state before attempting network synchronization.

Selecting another LoFi operation also retains the prior state until the replacement
operation has been fetched and durably saved. Exiting demo mode restores the same
last-good snapshot.

## Compatibility risks and fallback

LoFi response envelopes, pagination metadata, sparse tombstones, and authentication
recovery require continued validation against a consenting real account. Sidecar
must never record credentials or private QSO payloads in fixtures, logs, issues, or
diagnostics. If LoFi cannot be trusted or reached, export the current operation from
PoLo and import its ADIF file; failed imports leave the displayed and persisted
last-good score unchanged.
