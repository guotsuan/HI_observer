#!/usr/bin/env python3
"""Read an HI Observer .bin recording and plot waterfall plus spectrum."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
import struct
import warnings

import matplotlib.pyplot as plt
import numpy as np


BINARY_MAGIC = b"HIOBIN01"
BINARY_VERSION = 1
BINARY_HEADER_SIZE = 256
BINARY_ROW_HEADER_SIZE = 16
BINARY_ENDIAN_MARKER = 0x01020304


@dataclass(frozen=True)
class BinMetadata:
    """Metadata stored in an HI Observer versioned binary header."""

    version: int
    start_unix_ns: int
    end_unix_ns: int
    initial_center_frequency_hz: float
    sample_rate_hz: float
    channel_width_hz: float
    time_resolution_sec: float
    channel_count: int
    average_count: int
    pfb_taps: int
    lna_gain_db: float
    mix_gain_db: float
    vga_gain_db: float
    declared_row_count: int
    declared_elapsed_sec: float
    row_size: int

    @property
    def start_time_utc(self) -> datetime:
        return datetime.fromtimestamp(self.start_unix_ns * 1e-9, tz=timezone.utc)

    @property
    def end_time_utc(self) -> datetime:
        return datetime.fromtimestamp(self.end_unix_ns * 1e-9, tz=timezone.utc)


@dataclass(frozen=True)
class BinObservation:
    """Spectra and coordinates loaded from either binary format."""

    spectra: np.ndarray
    times_sec: np.ndarray
    center_frequencies_hz: np.ndarray
    sample_rate_hz: float
    channel_count: int
    metadata: BinMetadata | None


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


def parse_versioned_header(header: bytes) -> BinMetadata:
    """Decode and validate the fixed 256-byte little-endian header."""
    if len(header) != BINARY_HEADER_SIZE:
        raise ValueError("the versioned binary header is incomplete")
    if header[:8] != BINARY_MAGIC:
        raise ValueError("not an HI Observer versioned binary file")

    version, header_size, endian_marker = struct.unpack_from("<III", header, 8)
    if version != BINARY_VERSION:
        raise ValueError(f"unsupported HI Observer binary version {version}")
    if header_size != BINARY_HEADER_SIZE:
        raise ValueError(f"unsupported binary header size {header_size}")
    if endian_marker != BINARY_ENDIAN_MARKER:
        raise ValueError("invalid binary byte-order marker")

    start_unix_ns = struct.unpack_from("<q", header, 24)[0]
    initial_center_frequency_hz, sample_rate_hz = struct.unpack_from(
        "<dd", header, 32
    )
    channel_width_hz, time_resolution_sec = struct.unpack_from("<dd", header, 56)
    channel_count, average_count, pfb_taps, value_type = struct.unpack_from(
        "<IIII", header, 72
    )
    lna_gain_db, mix_gain_db, vga_gain_db = struct.unpack_from("<ddd", header, 88)
    row_header_size, value_size = struct.unpack_from("<II", header, 112)
    row_size, declared_row_count = struct.unpack_from("<QQ", header, 120)
    end_unix_ns = struct.unpack_from("<q", header, 136)[0]
    declared_elapsed_sec = struct.unpack_from("<d", header, 144)[0]

    expected_row_size = BINARY_ROW_HEADER_SIZE + channel_count * 4
    if channel_count <= 0:
        raise ValueError("binary header has no frequency channels")
    if value_type != 1 or value_size != 4:
        raise ValueError("binary file does not contain float32 linear-power values")
    if row_header_size != BINARY_ROW_HEADER_SIZE or row_size != expected_row_size:
        raise ValueError("binary header has an invalid row layout")
    if sample_rate_hz <= 0.0 or channel_width_hz <= 0.0:
        raise ValueError("binary header has invalid frequency metadata")

    return BinMetadata(
        version=version,
        start_unix_ns=start_unix_ns,
        end_unix_ns=end_unix_ns,
        initial_center_frequency_hz=initial_center_frequency_hz,
        sample_rate_hz=sample_rate_hz,
        channel_width_hz=channel_width_hz,
        time_resolution_sec=time_resolution_sec,
        channel_count=channel_count,
        average_count=average_count,
        pfb_taps=pfb_taps,
        lna_gain_db=lna_gain_db,
        mix_gain_db=mix_gain_db,
        vga_gain_db=vga_gain_db,
        declared_row_count=declared_row_count,
        declared_elapsed_sec=declared_elapsed_sec,
        row_size=row_size,
    )


def read_versioned_bin(path: Path, rows: int) -> BinObservation:
    """Read the self-describing HIOBIN01 format."""
    with path.open("rb") as source:
        metadata = parse_versioned_header(source.read(BINARY_HEADER_SIZE))

    file_size = path.stat().st_size
    data_size = file_size - BINARY_HEADER_SIZE
    complete_row_count, trailing_bytes = divmod(data_size, metadata.row_size)
    if trailing_bytes:
        warnings.warn(
            f"ignoring {trailing_bytes} trailing bytes from an incomplete final row",
            stacklevel=2,
        )
    if complete_row_count == 0:
        raise ValueError(f"{path} contains no complete spectra")
    if metadata.declared_row_count not in (0, complete_row_count):
        warnings.warn(
            f"header declares {metadata.declared_row_count} rows, but the file contains "
            f"{complete_row_count} complete rows; using the complete rows",
            stacklevel=2,
        )

    selected_count = complete_row_count if rows <= 0 else min(rows, complete_row_count)
    first_row = complete_row_count - selected_count
    row_dtype = np.dtype(
        [
            ("time", "<f8"),
            ("center_frequency", "<f8"),
            ("spectrum", "<f4", (metadata.channel_count,)),
        ]
    )
    records = np.fromfile(
        path,
        dtype=row_dtype,
        count=selected_count,
        offset=BINARY_HEADER_SIZE + first_row * metadata.row_size,
    )
    return BinObservation(
        spectra=np.asarray(records["spectrum"], dtype=np.float32).copy(),
        times_sec=np.asarray(records["time"], dtype=np.float64).copy(),
        center_frequencies_hz=np.asarray(
            records["center_frequency"], dtype=np.float64
        ).copy(),
        sample_rate_hz=metadata.sample_rate_hz,
        channel_count=metadata.channel_count,
        metadata=metadata,
    )


def read_legacy_bin(
    path: Path,
    channel_count: int,
    rows: int,
) -> np.ndarray:
    """Read the original headerless float32 format."""
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


def read_bin(
    path: Path,
    rows: int,
    *,
    legacy_center_frequency_hz: float | None,
    legacy_sample_rate_hz: float,
    legacy_channel_count: int,
    legacy_average_count: int,
    legacy_time_resolution_sec: float | None,
) -> BinObservation:
    """Auto-detect the self-describing format, with legacy raw fallback."""
    with path.open("rb") as source:
        magic = source.read(len(BINARY_MAGIC))
    if magic == BINARY_MAGIC:
        return read_versioned_bin(path, rows)

    if legacy_center_frequency_hz is None:
        raise ValueError(
            "legacy headerless .bin files require --center-frequency; "
            "also verify --sample-rate, --nch, and --average"
        )
    spectra = read_legacy_bin(path, legacy_channel_count, rows)
    time_resolution_sec = legacy_time_resolution_sec
    if time_resolution_sec is None:
        time_resolution_sec = (
            legacy_channel_count
            * legacy_average_count
            / (2.0 * legacy_sample_rate_hz)
        )
    return BinObservation(
        spectra=spectra,
        times_sec=np.arange(spectra.shape[0], dtype=np.float64)
        * time_resolution_sec,
        center_frequencies_hz=np.full(
            spectra.shape[0], legacy_center_frequency_hz, dtype=np.float64
        ),
        sample_rate_hz=legacy_sample_rate_hz,
        channel_count=legacy_channel_count,
        metadata=None,
    )


def print_metadata(observation: BinObservation) -> None:
    """Print the metadata that controls the plotted coordinates."""
    metadata = observation.metadata
    if metadata is None:
        print("Format: legacy headerless float32")
        print(f"Rows loaded: {observation.spectra.shape[0]}")
        print(f"Channels: {observation.channel_count}")
        return

    print(f"Format: HI Observer binary v{metadata.version}")
    print(f"Observation start (UTC): {metadata.start_time_utc.isoformat()}")
    print(f"Observation end (UTC): {metadata.end_time_utc.isoformat()}")
    print(f"Recorded elapsed time: {metadata.declared_elapsed_sec:.6f} s")
    print(f"Rows in file header: {metadata.declared_row_count}")
    print(f"Rows loaded: {observation.spectra.shape[0]}")
    print(f"Channels: {metadata.channel_count}")
    print(f"Initial center frequency: {metadata.initial_center_frequency_hz:.6f} Hz")
    print(f"Sample rate: {metadata.sample_rate_hz:.6f} Hz")
    print(f"Channel width: {metadata.channel_width_hz:.6f} Hz")
    print(f"Nominal row interval: {metadata.time_resolution_sec:.9f} s")
    print(f"Average count: {metadata.average_count}")
    print(f"PFB taps: {metadata.pfb_taps}")
    print(
        "Gains: "
        f"LNA={metadata.lna_gain_db:g} dB, "
        f"MIX={metadata.mix_gain_db:g} dB, "
        f"VGA={metadata.vga_gain_db:g} dB"
    )


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Plot a raw HI Observer .bin recording."
    )
    parser.add_argument("input", type=Path, help="input .bin file")
    parser.add_argument(
        "--center-frequency",
        type=float,
        metavar="HZ",
        help="center frequency in Hz; required only for legacy headerless files",
    )
    parser.add_argument(
        "--sample-rate",
        type=float,
        default=6e6,
        metavar="HZ",
        help="legacy sample rate in Hz (default: 6e6)",
    )
    parser.add_argument(
        "--nch", type=int, default=4096, help="legacy channel count (default: 4096)"
    )
    parser.add_argument(
        "--average",
        type=int,
        default=128,
        help="legacy number of spectra averaged per saved row (default: 128)",
    )
    parser.add_argument(
        "--time-resolution",
        type=float,
        metavar="SECONDS",
        help="legacy saved-row interval; otherwise derived from legacy parameters",
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
    parser.add_argument("--title", help="plot title")
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
    observation = read_bin(
        args.input,
        rows_to_read,
        legacy_center_frequency_hz=args.center_frequency,
        legacy_sample_rate_hz=args.sample_rate,
        legacy_channel_count=args.nch,
        legacy_average_count=args.average,
        legacy_time_resolution_sec=args.time_resolution,
    )
    print_metadata(observation)

    center_frequency_hz = float(observation.center_frequencies_hz[-1])
    channel_width_hz = observation.sample_rate_hz / observation.channel_count
    if not np.allclose(
        observation.center_frequencies_hz,
        center_frequency_hz,
        rtol=0.0,
        atol=channel_width_hz / 2.0,
    ):
        warnings.warn(
            "center frequency changed within the selected rows; the waterfall is "
            "displayed on the final row's frequency grid",
            stacklevel=2,
        )
    frequency_min_hz = center_frequency_hz - observation.sample_rate_hz / 2.0
    frequency_max_hz = center_frequency_hz + observation.sample_rate_hz / 2.0
    frequencies_hz = frequency_min_hz + np.arange(
        observation.channel_count
    ) * channel_width_hz
    title = args.title
    if title is None:
        if observation.metadata is None:
            title = "HI Observation"
        else:
            start = observation.metadata.start_time_utc.strftime("%Y-%m-%d %H:%M:%S UTC")
            title = f"HI Observation — {start}"

    plot_observation(
        observation.spectra,
        observation.times_sec,
        frequencies_hz,
        (frequency_min_hz, frequency_max_hz),
        waterfall_rows=args.rows,
        spectrum_average_rows=args.spectrum_average,
        title=title,
        output=args.output,
        show=not args.no_show,
    )


if __name__ == "__main__":
    main()
