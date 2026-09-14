#!/usr/bin/env python3
"""Read an HI Observer FITS table and plot waterfall plus spectrum."""

from __future__ import annotations

import argparse
from pathlib import Path
import warnings

import numpy as np
from astropy.io import fits

from plot_bin import plot_observation


def read_fits(
    path: Path,
    rows: int,
) -> tuple[np.ndarray, np.ndarray, np.ndarray, tuple[float, float]]:
    with fits.open(path, memmap=True) as hdus:
        hdus.verify("exception")
        if "SPECTRA" not in hdus:
            raise ValueError(f"{path} has no SPECTRA binary-table extension")
        table_hdu = hdus["SPECTRA"]
        required_columns = {"TIME", "FREQUENCY", "SPECTRUM"}
        available_columns = set(table_hdu.columns.names)
        missing_columns = required_columns - available_columns
        if missing_columns:
            missing = ", ".join(sorted(missing_columns))
            raise ValueError(f"FITS SPECTRA table is missing: {missing}")
        if len(table_hdu.data) == 0:
            raise ValueError(f"{path} contains no spectra")

        selected = table_hdu.data[-rows:] if rows > 0 else table_hdu.data
        times_sec = np.asarray(selected["TIME"], dtype=np.float64).copy()
        center_frequencies_hz = np.asarray(
            selected["FREQUENCY"], dtype=np.float64
        ).copy()
        spectra = np.asarray(selected["SPECTRUM"], dtype=np.float32).reshape(
            len(selected), -1
        )
        header = table_hdu.header.copy()

    channel_count = spectra.shape[1]
    declared_channels = int(header.get("NCHANS", channel_count))
    if declared_channels != channel_count:
        raise ValueError(
            f"NCHANS={declared_channels}, but SPECTRUM contains {channel_count} channels"
        )

    sample_rate_hz = float(
        header.get("SAMPRATE", header.get("BANDWID", 0.0))
    )
    channel_width_hz = float(
        header.get(
            "CHAN_BW",
            sample_rate_hz / channel_count if sample_rate_hz > 0 else 0.0,
        )
    )
    if sample_rate_hz <= 0 or channel_width_hz <= 0:
        raise ValueError("FITS header must provide positive SAMPRATE/BANDWID and CHAN_BW")

    center_frequency_hz = float(center_frequencies_hz[-1])
    if not np.allclose(
        center_frequencies_hz,
        center_frequency_hz,
        rtol=0.0,
        atol=channel_width_hz / 2.0,
    ):
        warnings.warn(
            "Center frequency changed within the selected rows; the waterfall is "
            "displayed on the final row's frequency grid.",
            stacklevel=2,
        )
    frequency_min_hz = center_frequency_hz - sample_rate_hz / 2.0
    frequency_max_hz = center_frequency_hz + sample_rate_hz / 2.0
    frequencies_hz = frequency_min_hz + np.arange(channel_count) * channel_width_hz
    return (
        spectra,
        times_sec,
        frequencies_hz,
        (frequency_min_hz, frequency_max_hz),
    )


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Plot an HI Observer FITS binary-table recording."
    )
    parser.add_argument("input", type=Path, help="input .fits or .fit file")
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
    spectra, times_sec, frequencies_hz, band_edges_hz = read_fits(
        args.input, rows_to_read
    )
    plot_observation(
        spectra,
        times_sec,
        frequencies_hz,
        band_edges_hz,
        waterfall_rows=args.rows,
        spectrum_average_rows=args.spectrum_average,
        title=args.title,
        output=args.output,
        show=not args.no_show,
    )


if __name__ == "__main__":
    main()
