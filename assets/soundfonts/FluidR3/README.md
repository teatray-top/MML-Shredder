# FluidR3 GM

The unmodified General MIDI bank by Frank Wen, release 3.1, supplies the
16 additional built-in preview instruments in MML 세단기. YDP Grand remains
the default piano, for a total of 17 selectable instruments.

- Source archive: https://deb.debian.org/debian/pool/main/f/fluid-soundfont/fluid-soundfont_3.1.orig.tar.gz
- Archive SHA-256: `2621acaa1c78e4abdb24bdd163230cc577e61276936d6aa6e3180582142f0343`
- File: `FluidR3_GM.sf2`, 148,398,306 bytes
- SF2 SHA-256: `74594e8f4250680adf590507a306655a299935343583256f3b722c48a1bc1cb0`
- License: MIT. `LICENSE.txt` and `upstream-README.txt` are unmodified files
  from that archive, originally named `COPYING` and `README`.

RustySynth renders the bank's sample loops, envelopes and filters. Each part
has a separate synthesizer so identical notes or program changes in one part
cannot affect another. Reverb and chorus are disabled; the output is mixed to
mono. Instrument choices affect preview only and default to YDP piano for a
newly loaded score. Program changes apply to subsequent note attacks.

The bank is embedded when building but deliberately excluded from Git.
Building from source requires it at `assets/soundfonts/FluidR3/FluidR3_GM.sf2`.
Release executables need no external SoundFont or VST installation. The
release includes this README and the original license and attribution in
`docs/instruments`.
