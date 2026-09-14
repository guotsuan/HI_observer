#!/usr/bin/env python3
"""Read an HI Observer .bin recording and plot waterfall plus spectrum."""

from __future__ import annotations

import argparse
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np


def positive_power_db(values: np.ndarray) -> np.ndarray:
    """Convert linear power to dB while keeping zero values finite."""
    floor = np.finfo(np.float32).tiny
    return 10.0 * np.log10(np.maximum(values, floor))


def average_last_spectra(spectra: np.ndarray, row_count: int) -> np.ndarray:
    """Average the last row_count spectra in linear-power space."""
    if row_count <= 0:
        raise ValueError("--spectrum-average must be positive")
    if row_count > spectra.shape[0]:
        raise ValueError(
            f"cannot average {row_count} rows; the file contains only "
            f"{spectra.shape[0]} rows"
        )
    return np.mean(spectra[-row_count:], axis=0, dtype=np.float64)


def plot_observation(
    spectra: np.ndarray,
    times_sec: np.ndarray,
    frequencies_hz: np.ndarray,
    band_edges_hz: tuple[float, float],
    *,
    waterfall_rows: int,
    spectrum_average_rows: int,
    title: str,
    output: Path | None,
    show: bool,
) -> None:
    """Plot data in the same waterfall/spectrum arrangement as channelize."""
    spectra = np.asarray(spectra, dtype=np.float32)
    if spectra.ndim != 2 or spectra.shape[0] == 0 or spectra.shape[1] == 0:
        raise ValueError("spectra must be a non-empty 2-D array")

    times_sec = np.asarray(times_sec, dtype=np.float64)
    frequencies_hz = np.asarray(frequencies_hz, dtype=np.float64)
    if times_sec.shape != (spectra.shape[0],):
        raise ValueError("one time value is required for every spectrum")
    if frequencies_hz.shape != (spectra.shape[1],):
        raise ValueError("one frequency value is required for every channel")

    waterfall_spectra = spectra[-waterfall_rows:] if waterfall_rows > 0 else spectra
    waterfall_times = times_sec[-waterfall_rows:] if waterfall_rows > 0 else times_sec
    averaged_spectrum = average_last_spectra(spectra, spectrum_average_rows)

    relative_times = waterfall_times - waterfall_times[-1]
    if len(relative_times) > 1:
        time_step = float(np.median(np.diff(relative_times)))
    else:
        time_step = 1.0
    time_min = float(relative_times[0] - time_step / 2.0)
    time_max = float(relative_times[-1] + time_step / 2.0)
    frequency_min_mhz = band_edges_hz[0] / 1e6
    frequency_max_mhz = band_edges_hz[1] / 1e6
    frequencies_mhz = frequencies_hz / 1e6
    waterfall_db = positive_power_db(waterfall_spectra)
    spectrum_db = positive_power_db(averaged_spectrum)

    figure, (waterfall_axis, spectrum_axis) = plt.subplots(
        2,
        1,
        figsize=(14, 8),
        sharex=True,
        gridspec_kw={"height_ratios": (1, 1)},
        constrained_layout=True,
    )
    image = waterfall_axis.imshow(
        waterfall_db,
        origin="lower",
        aspect="auto",
        extent=(frequency_min_mhz, frequency_max_mhz, time_min, time_max),
        cmap="viridis",
        interpolation="nearest",
    )
    waterfall_axis.invert_yaxis()
    waterfall_axis.set_ylabel("Time (s)", fontsize=14)
    waterfall_axis.tick_params(labelsize=14)
    waterfall_axis.set_title(title, fontsize=14)
    colorbar = figure.colorbar(image, ax=waterfall_axis, pad=0.01)
    colorbar.set_label("Power (dB)", fontsize=14)
    colorbar.ax.tick_params(labelsize=14)

    spectrum_axis.plot(frequencies_mhz, spectrum_db, color="blue", linewidth=0.8)
    spectrum_axis.set_xlim(frequency_min_mhz, frequency_max_mhz)
    spectrum_axis.set_xlabel("Frequency (MHz)", fontsize=14)
    spectrum_axis.set_ylabel("Power (dB)", fontsize=14)
    spectrum_axis.tick_params(labelsize=14)
    spectrum_axis.grid(True, which="both", linewidth=0.4, alpha=0.45)
    if spectrum_average_rows > 1:
        spectrum_axis.set_title(
            f"Average of last {spectrum_average_rows} rows", fontsize=14
        )

    if output is not None:
        figure.savefig(output, dpi=160)
        print(f"Saved plot: {output}")
    if show:
        plt.show()
    else:
        plt.close(figure)


def read_bin(
    path: Path,
    channel_count: int,
    rows: int,
) -> np.ndarray:
    if channel_count <= 0:
        raise ValueError("--nch must be positive")
    values = np.fromfile(path, dtype="<f4")
    if values.size == 0:
        raise ValueError(f"{path} contains no spectra")
    if values.size % channel_count != 0:
        raise ValueError(
            f"{path} contains {values.size} float32 values, which is not divisible "
            f"by nch={channel_count}"
        )
    spectra = values.reshape(-1, channel_count)
    return spectra[-rows:] if rows > 0 else spectra


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Plot a raw HI Observer .bin recording."
    )
    parser.add_argument("input", type=Path, help="input .bin file")
    parser.add_argument(
        "--center-frequency",
        type=float,
        required=True,
        metavar="HZ",
        help="observation center frequency in Hz",
    )
    parser.add_argument(
        "--sample-rate",
        type=float,
        default=6e6,
        metavar="HZ",
        help="sample rate in Hz (default: 6e6)",
    )
    parser.add_argument("--nch", type=int, default=512, help="channel count")
    parser.add_argument(
        "--average",
        type=int,
        default=128,
        help="number of spectra averaged per saved row",
    )
    parser.add_argument(
        "--time-resolution",
        type=float,
        metavar="SECONDS",
        help="saved-row interval; defaults to nch*average/(2*sample_rate)",
    )
    parser.add_argument(
        "--rows",
        type=int,
        default=128,
        help="number of most recent rows to plot; <=0 plots all rows",
    )
    parser.add_argument(
        "--spectrum-average",
        type=int,
        default=1,
        metavar="N",
        help="average the last N rows for the lower spectrum (default: 1)",
    )
    parser.add_argument("--title", default="HI Observation", help="plot title")
    parser.add_argument("--output", type=Path, help="save the plot to an image file")
    parser.add_argument(
        "--no-show", action="store_true", help="do not open an interactive plot window"
    )
    return parser.parse_args()


def main() -> None:
    args = parse_arguments()
    rows_to_read = (
        0
        if args.rows <= 0
        else max(args.rows, args.spectrum_average)
    )
    spectra = read_bin(args.input, args.nch, rows_to_read)
    time_resolution = args.time_resolution
    if time_resolution is None:
        time_resolution = args.nch * args.average / (2.0 * args.sample_rate)
    times_sec = np.arange(spectra.shape[0], dtype=np.float64) * time_resolution
    channel_width_hz = args.sample_rate / args.nch
    frequency_min_hz = args.center_frequency - args.sample_rate / 2.0
    frequency_max_hz = args.center_frequency + args.sample_rate / 2.0
    frequencies_hz = frequency_min_hz + np.arange(args.nch) * channel_width_hz

    plot_observation(
        spectra,
        times_sec,
        frequencies_hz,
        (frequency_min_hz, frequency_max_hz),
        waterfall_rows=args.rows,
        spectrum_average_rows=args.spectrum_average,
        title=args.title,
        output=args.output,
        show=not args.no_show,
    )


if __name__ == "__main__":
    main()
