# GeissOxide

Audio visualizers for the desktop, in pure Rust:

- **GeissOxide** — a port of Ryan Geiss' 1998-2022 Winamp plug-in / screensaver
  ([geissomatik/geiss](https://github.com/geissomatik/geiss), BSD-3): 25 warp maps, palettes,
  waveforms, beat-driven scene changes.
- **MilkDrop** — a MilkDrop 1 compatible engine (`.milk` presets, NS-EEL scripts, custom waves
  and shapes, motion vectors, video echo). MilkDrop 2 pixel-shader presets are skipped by default.
- **Chladni** — an original cymatics engine: the spectrum drives the standing-wave modes of a
  vibrating plate and sand grains gather on its nodal lines, drawing Chladni figures that re-form
  with the music; beats knock the plate and scatter the sand.
- **Tonnetz** — harmony made visible: the sound is folded into the 12 pitch classes and drawn on
  Euler's Tonnetz (right = fifth, up-right = major third), where every major triad is an
  up-pointing triangle and every minor triad a down-pointing one. Chords fill their triangle, the
  camera follows the key and the progression leaves a fading path.

The sound comes from the sound card: an **external** input (microphone, line-in, USB interface)
or the **internal** loopback of what the computer is playing. Linux amd64 and macOS (universal).
The interface is in English or French (auto-detected, English fallback).

## Install

Download the latest build from the [Releases](../../releases) page:

- **macOS** — `GeissOxide-macos-universal.zip`; unzip and run `GeissOxide.app` (MilkDrop presets included).
- **Linux amd64** — `geissoxide-linux-amd64`; `chmod +x` it and run.

Building from source instead? See [CONTRIBUTING.md](CONTRIBUTING.md).

## Run

```text
geissoxide [--engine geissoxide|milkdrop|chladni|tonnetz] [--device NAME|INDEX] [--list-devices] [--presets DIR]
      [--res WxH] [--fullscreen] [--gain F] [--preset-duration S] [--allow-shader-presets]
      [--input device|test] [--frames N --screenshot out.png]
```

Keys: `Esc` quit · `F` fullscreen · `Tab` switch engine · `Space`/`→` next preset, map or figure ·
`←` previous preset · `L` lock preset · `D` next input device · `+`/`-` gain · `H` help.

`--input test` renders from a built-in synthetic signal; with `--frames N --screenshot FILE` it
saves the last frame and exits, which is handy for checking a machine without a sound source.

## Choosing the audio source

`geissoxide --list-devices` prints the inputs; pick one with `--device` (index or part of the name).
On macOS, launching `GeissOxide.app` from the Finder shows a native list of the inputs instead; the
last choice is preselected next time. `D` switches to the next input while running.

**macOS, system audio (internal):** macOS has no loopback input, so install a virtual device such as
[BlackHole](https://github.com/ExistentialAudio/BlackHole) (`brew install --cask blackhole-2ch`).
In *Audio MIDI Setup* create a **Multi-Output Device** with your speakers and BlackHole, select it
as the system output, then run `geissoxide --device blackhole`. The first launch asks for microphone
permission.

**Linux, system audio (internal):** PulseAudio / PipeWire expose every output as a *monitor*
source. Either make the monitor the default source and use the ALSA `pulse` device:

```bash
pactl list short sources          # find e.g. alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
pactl set-default-source alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
geissoxide --device pulse
```

or point the ALSA plug-in at it for one run: `PULSE_SOURCE=<sink>.monitor geissoxide --device pulse`.

## Presets

The macOS `.app` ships with the [presets-milkdrop-original](https://github.com/projectM-visualizer/presets-milkdrop-original)
pack already bundled. On Linux, point `--presets DIR` at any directory of `.milk` files, e.g. a
clone of that same repository. Presets that need MilkDrop 2 pixel shaders are skipped unless
`--allow-shader-presets` is given (they then render without their shaders).

## Licence

BSD-3-Clause. GeissOxide is a Rust port of Geiss and MilkDrop, © Ryan Geiss / Nullsoft (BSD-3);
the MilkDrop geometry follows [butterchurn](https://github.com/jberg/butterchurn) (MIT).
