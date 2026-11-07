//! Bit-exact parity of the fixed-point FFT with pyaec's own speexdsp.
//!
//! `../golden/fft/fft_<n>.i16` comes from `harness/gen_fft_golden.py`, which calls
//! `spx_fft` / `spx_ifft` inside pyaec's libaec.dylib: full-range, sparse, tonal,
//! quiet frames and frames holding -32768, including the inputs on which the C
//! transform overflows. Missing data fails the test.

mod common;

use std::fs;

use common::{golden, read_i16};
use speexdsp_aec::fft::SpeexFft;

#[test]
fn spx_fft_and_spx_ifft_are_bit_exact_with_c() {
    let mut checked = 0;
    for entry in fs::read_dir(golden("fft")).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().into_string().unwrap();
        let n: usize = name.trim_start_matches("fft_").trim_end_matches(".i16").parse().unwrap();
        let data = read_i16(&entry.path());
        let mut fft = SpeexFft::new(n).unwrap();
        for (r, record) in data.chunks_exact(4 * n).enumerate() {
            let (input, scaled, spectrum, time) = (&record[..n], &record[n..2 * n], &record[2 * n..3 * n], &record[3 * n..]);
            let mut buffer = input.to_vec();
            let mut out = vec![0i16; n];
            fft.forward(&mut buffer, &mut out);
            assert_eq!(out, spectrum, "spx_fft, n = {n}, record {r}");
            assert_eq!(buffer, scaled, "spx_fft's in-place rescaling, n = {n}, record {r}");
            fft.inverse(input, &mut out);
            assert_eq!(out, time, "spx_ifft, n = {n}, record {r}");
            checked += 1;
        }
    }
    assert!(checked > 0);
    eprintln!("{checked} frames bit-exact");
}
