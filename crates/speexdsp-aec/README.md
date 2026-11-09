# speexdsp-aec

A safe Rust port (`#![forbid(unsafe_code)]`) of the acoustic echo canceller of
speexdsp 1.2.1: `mdf.c` in its fixed-point build, with KISS FFT and the two-path
filter. It is the build that pyaec 1.0.1 ships, and for the same input its output is
the same, bit for bit. The port also refuses configurations speexdsp would mishandle,
checks buffer lengths, and lets the sampling rate be set; pyaec never sets it, so its
canceller always runs with 8 kHz settings.

The port was made, tested against pyaec and mutation-tested in its own repository,
`materkey/speexdsp-aec` (private). That repository keeps the record of the port: the
audit, the benchmarks and the mutation results. It also has the generators of the
golden data: `harness/gen_cases.py` and `harness/gen_fft_golden.py`, run with an
interpreter that has pyaec 1.0.1.

## Tests

```bash
cargo test -p speexdsp-aec --lib --test properties --test cli --test adapted_golden   # no data needed
cargo test -p speexdsp-aec --doc
SPEEXDSP_AEC_GOLDEN=/path/to/golden cargo test -p speexdsp-aec --test parity --test fft_parity
```

The parity tests compare the port with pyaec's own output over 80 generated cases and
610 resets, and the FFT with `spx_fft`/`spx_ifft` over 4200 frames of 14 sizes. They
fail when the golden data is missing, so a test that checked nothing cannot pass.

`adapted_golden` is the part of that comparison CI can run: seven cases generated in
the test, long enough for the filter to adapt, against SHA-256 digests of pyaec's
output kept in `tests/fixtures/`; the test's documentation says how they were made.

## Licence

BSD-3-Clause, as speexdsp and KISS FFT; see `LICENSE`.
