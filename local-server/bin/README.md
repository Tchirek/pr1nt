# Bundled binaries

The local print server shells out to [SumatraPDF](https://www.sumatrapdfreader.org/)
for silent printing. The executable is **not** committed to this repository (it is a
third-party binary and is excluded by the public-safety audit).

To set it up:

1. Download the portable **SumatraPDF** build for Windows.
2. Either place `SumatraPDF.exe` in this directory, or point `SUMATRA_PDF_PATH`
   in `local-server/.env` at wherever you installed it.

The server resolves `SUMATRA_PDF_PATH` relative to the executable, so the default
value `SumatraPDF` will find `bin/SumatraPDF.exe` next to the built binary.
