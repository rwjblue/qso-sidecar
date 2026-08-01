# NAQP CW rules model

QSO Sidecar's scoring model is versioned as `2026-08-cw`. It is intentionally scoped
to the August 2026 North American QSO Party CW event, using the official
[2026 NAQP rules](https://ncjweb.com/NAQP-Rules.pdf) and the official
[paper log and multiplier checklist](https://ncjweb.com/NAQP-Paper-Log-Form.pdf).

## Implemented assumptions

- Contest period: 18:00 UTC August 1 through 06:00 UTC August 2, 2026. The end is
  exclusive.
- Mode: CW only.
- Bands: 160, 80, 40, 20, 15, and 10 meters. Cross-mode and cross-band duplicates do
  not affect CW scoring; a repeated callsign on the same band is a duplicate.
- Exchange: operator name plus a recognized North American multiplier, or `DX` for a
  station outside North America.
- Score: valid contacts multiplied by the sum of distinct multipliers worked on each
  band. A valid non-North-American `DX` contact counts as a QSO but not a multiplier.
- Operating time is estimated from valid-QSO timestamps. A gap of at least 31 minutes
  marks off time. The dashboard reports claimed score only; log-checking penalties and
  adjudication remain outside its scope.

The API includes the rules version and a reason for every normalized contact. Records
can be credited, duplicate, incomplete-exchange, wrong-mode, ineligible-band,
outside-period, or tombstone. Source records that cannot become contacts are retained
as diagnostics with event/non-contact, missing-call, missing-timestamp, or malformed
reasons instead of disappearing silently.

## Multiplier catalog

The matrix contains all 111 checklist rows: United States and the District of
Columbia, Canadian provinces/territories, and the other listed North American
countries/entities. It therefore has 111 stable row identifiers but 110 distinct
printed codes: `HI` is printed for both Hawaii and the Dominican Republic.

Those two rows are kept separate internally as `US-HI` and `DXCC-HI`. Sidecar resolves
the shared printed code using callsign and country context, so working Hawaii and the
Dominican Republic on the same band earns two multipliers. If the context cannot
disambiguate the code, the exchange remains unresolved rather than receiving guessed
credit.

Every row carries a display name and a group (`us_dc`, `canada`, or
`other_north_america`). The browser receives a complete server-built six-band matrix,
including needed, possible spotted, verified spotted, unresolved, and worked states.
