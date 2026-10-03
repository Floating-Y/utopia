# calamine 0.36.1 compatibility patch

Source: the published [calamine 0.36.1 crate](https://crates.io/crates/calamine/0.36.1),
crate SHA-256 `5fa68281b1a76b54a62156474adb06bb380a67e07dd60656e3217152b42183f3`,
upstream commit `0a24c2a9f1e38c0932c1299e633270dc730db505`.
The source, manifest, examples, benchmarks, README, changelog and MIT license are
copied from that release. No Cargo cache files are modified.
The examples README has its extra final blank line removed for diff checks.

Only three upstream source files differ:

- `src/formats.rs`: share the XLSX/XLS built-in classification; recognize
  27–31, 36, 50, 51, 54, 57, 58 as dates, and 32/33 as clock time.
- `src/datatype.rs`: preserve the clock-time category without changing the
  stored serial value or date epoch. Existing DateTime/TimeDelta behavior stays.
- `src/xlsb/mod.rs`: honor explicit formats before built-in classification,
  matching XLSX/XLS and preventing the new IDs from overriding explicit codes.

The [Microsoft number format tables](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.spreadsheet.numberingformat)
list 34, 35, 52, 53, 55, 56 as dates in some locales and clock times in others.
Without an explicit format code or reliable locale these IDs remain numeric;
guessing from the serial value would invent calendar dates.
[MS-XLS IFmt](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-xls/9017e247-7995-4a9c-96e8-950df24735a2)
uses the same built-in table. XLS FORMAT records cannot legally redefine these
low IDs; XLSX explicit numFmt codes continue to take precedence.

Public parse regressions for XLSX and CFB/BIFF8 XLS live in
`crates/utopia-ingest/tests/spreadsheet_dates.rs`, including XLS NUMBER, RK,
MULRK and numeric formula caches. XLSB shares the dependency
classification but is not covered by these public parse tests.

Upgrade the exact workspace version pin, remove the patch and this directory once a published calamine release
provides equivalent classification and time metadata, then rerun those tests.
This is ISO normalization, not an Excel display-formatting engine. Previously
ingested affected documents need reprocessing to regenerate their text/chunks.
